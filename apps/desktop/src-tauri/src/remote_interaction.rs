//! Device-local interactions for a single remote plugin invocation.
//! No filesystem paths supplied by a browser are ever opened on the host.
use axum::body::Bytes;
use base64::Engine as _;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak, mpsc};
use std::time::{Duration, Instant};
use wonderland_plugin_protocol::PluginError;

const TTL: Duration = Duration::from_secs(300);
const WAIT: Duration = Duration::from_secs(45);
const MAX_BYTES: usize = 64 * 1024 * 1024;
const CHUNK: u64 = 1024 * 1024;

fn error(code: &str, message: &str) -> PluginError {
    PluginError {
        code: code.into(),
        message: message.into(),
        details: None,
    }
}

fn safe_selected_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !matches!(name, "." | "..")
        && !name
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\' | ':'))
}

struct Action {
    owner: String,
    plugin: String,
    kind: String,
    payload: Value,
    expires: Instant,
    reply: Option<mpsc::SyncSender<Result<Value, PluginError>>>,
    bytes: Option<Bytes>,
}

#[derive(Default)]
pub(crate) struct RemoteBroker {
    closed: AtomicBool,
    actions: Mutex<HashMap<String, Action>>,
}

impl RemoteBroker {
    fn prune(actions: &mut HashMap<String, Action>) {
        actions.retain(|_, action| action.expires > Instant::now());
    }

    fn insert(&self, action: Action) -> Result<String, PluginError> {
        let mut actions = self.actions.lock().unwrap();
        if self.closed.load(Ordering::Acquire) {
            return Err(error("CANCELLED", "远程连接已关闭。"));
        }
        Self::prune(&mut actions);
        let bytes: usize = actions
            .values()
            .map(|a| a.bytes.as_ref().map_or(0, |b| b.len()))
            .sum();
        if actions.len() >= 64 || bytes + action.bytes.as_ref().map_or(0, |b| b.len()) > MAX_BYTES {
            return Err(error(
                "RESOURCE_LIMIT",
                "待处理的远程文件过多，请先下载或清除文件。",
            ));
        }
        let id =
            crate::web_access::random_key().map_err(|_| error("INTERNAL", "无法生成交互标识。"))?;
        actions.insert(id.clone(), action);
        Ok(id)
    }

    pub(crate) fn list(&self, owner: &str) -> Value {
        let mut actions = self.actions.lock().unwrap();
        Self::prune(&mut actions);
        json!(
            actions
                .iter()
                .filter(|(_, a)| a.owner == owner)
                .map(|(id, a)| json!({
                    "id": id, "pluginId": a.plugin, "kind": a.kind, "payload": a.payload,
                }))
                .collect::<Vec<_>>()
        )
    }

    pub(crate) fn reply(&self, owner: &str, id: &str, value: Value) -> Result<(), PluginError> {
        let mut actions = self.actions.lock().unwrap();
        Self::prune(&mut actions);
        if !actions.get(id).is_some_and(|a| a.owner == owner) {
            return Err(error("UNAUTHORIZED", "交互已过期或不属于当前设备。"));
        }
        let action = actions.remove(id).unwrap();
        if let Some(sender) = action.reply {
            let result = if value.get("cancelled").and_then(Value::as_bool) == Some(true)
                && action.kind != "pick"
            {
                Err(error("CANCELLED", "设备上的操作已取消。"))
            } else {
                Ok(value)
            };
            let _ = sender.send(result);
        }
        Ok(())
    }

    pub(crate) fn download(&self, owner: &str, id: &str) -> Result<Bytes, PluginError> {
        let mut actions = self.actions.lock().unwrap();
        Self::prune(&mut actions);
        actions
            .get(id)
            .filter(|a| a.owner == owner)
            .and_then(|a| a.bytes.clone())
            .ok_or_else(|| error("UNAUTHORIZED", "下载已过期或不属于当前设备。"))
    }

    pub(crate) fn plugin(&self, owner: &str, id: &str) -> Option<String> {
        let mut actions = self.actions.lock().unwrap();
        Self::prune(&mut actions);
        actions
            .get(id)
            .filter(|a| a.owner == owner)
            .map(|a| a.plugin.clone())
    }

    pub(crate) fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.actions.lock().unwrap().clear();
    }
}

struct SelectedFile {
    name: String,
    size: u64,
    mime: String,
}

pub(crate) struct RemoteContext {
    broker: Arc<RemoteBroker>,
    owner: String,
    plugin: String,
    cancelled: AtomicBool,
    files: Mutex<HashMap<String, SelectedFile>>,
    waiting: Mutex<Vec<String>>,
    children: Mutex<Vec<Weak<RemoteContext>>>,
}

impl RemoteContext {
    pub(crate) fn for_plugin(&self, plugin: &str) -> Arc<Self> {
        let child = Arc::new(Self::new(
            self.broker.clone(),
            self.owner.clone(),
            plugin.into(),
        ));
        let mut children = self.children.lock().unwrap();
        if self.cancelled.load(Ordering::Acquire) {
            child.cancel();
        } else {
            children.retain(|child| child.strong_count() > 0);
            children.push(Arc::downgrade(&child));
        }
        child
    }
    pub(crate) fn new(broker: Arc<RemoteBroker>, owner: String, plugin: String) -> Self {
        Self {
            broker,
            owner,
            plugin,
            cancelled: AtomicBool::new(false),
            files: Mutex::new(HashMap::new()),
            waiting: Mutex::new(Vec::new()),
            children: Mutex::new(Vec::new()),
        }
    }

    pub(crate) fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        for child in self.children.lock().unwrap().drain(..) {
            if let Some(child) = child.upgrade() {
                child.cancel();
            }
        }
        let ids = self.waiting.lock().unwrap();
        let mut actions = self.broker.actions.lock().unwrap();
        for id in ids.iter() {
            actions.remove(id);
        }
    }

    fn action(
        &self,
        kind: &str,
        payload: Value,
        bytes: Option<Vec<u8>>,
        reply: Option<mpsc::SyncSender<Result<Value, PluginError>>>,
    ) -> Result<String, PluginError> {
        // Lock order matches cancel(), including the cancellation check and insertion.
        let mut waiting = self.waiting.lock().unwrap();
        if self.cancelled.load(Ordering::Acquire) {
            return Err(error("CANCELLED", "远程调用已结束。"));
        }
        let is_waiting = reply.is_some();
        let id = self.broker.insert(Action {
            owner: self.owner.clone(),
            plugin: self.plugin.clone(),
            kind: kind.into(),
            payload,
            expires: Instant::now() + if is_waiting { WAIT } else { TTL },
            reply,
            bytes: bytes.map(Bytes::from),
        })?;
        if is_waiting {
            waiting.push(id.clone());
        }
        Ok(id)
    }

    fn ask(&self, kind: &str, payload: Value) -> Result<Value, PluginError> {
        let (tx, rx) = mpsc::sync_channel(1);
        let id = self.action(kind, payload, None, Some(tx))?;
        let result = rx
            .recv_timeout(WAIT)
            .map_err(|_| error("CANCELLED", "远程文件操作已取消、超时或连接已关闭。"));
        self.waiting.lock().unwrap().retain(|item| item != &id);
        self.broker.actions.lock().unwrap().remove(&id);
        result?
    }

    pub(crate) fn service(&self, method: &str, params: &Value) -> Result<Value, PluginError> {
        match method {
            "core.files.pick" => self.pick(params),
            "core.files.read" => self.read(params),
            "core.files.export" => {
                let name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|name| crate::plugin_runtime::safe_export_name(name))
                    .ok_or_else(|| error("INVALID_INPUT", "导出文件名无效。"))?;
                let encoded = params
                    .get("contentBase64")
                    .and_then(Value::as_str)
                    .filter(|v| v.len() <= 28 * 1024 * 1024)
                    .ok_or_else(|| error("RESOURCE_LIMIT", "导出文件不能超过 20 MiB。"))?;
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .map_err(|_| error("INVALID_INPUT", "导出内容无效。"))?;
                if bytes.len() > 20 * 1024 * 1024 {
                    return Err(error("RESOURCE_LIMIT", "导出文件不能超过 20 MiB。"));
                }
                self.action(
                    "download",
                    json!({"name": name, "size": bytes.len()}),
                    Some(bytes),
                    None,
                )?;
                Ok(json!({"path": format!("浏览器下载/{name}")}))
            }
            "core.files.export_dir" => Ok(json!({"path":"浏览器下载"})),
            "core.files.reveal_own"
                if params.get("scope").and_then(Value::as_str) == Some("exports") =>
            {
                self.action("notice", json!({"message":"导出文件在当前设备的远程文件面板中。点击下载后，由浏览器保存到此设备。"}), None, None)?;
                Ok(json!({"ok":true}))
            }
            "core.files.reveal_own" => Err(error(
                "REMOTE_UNSUPPORTED",
                "远程设备无法打开宿主机数据目录，请在宿主机本地操作。",
            )),
            "core.browser.open_official" => {
                let url = crate::plugin_runtime::official_url(params)?;
                self.action("open_url", json!({"url":url}), None, None)?;
                Ok(json!({"ok":true}))
            }
            _ => Err(error("REMOTE_UNSUPPORTED", "此宿主交互暂不支持远程调用。")),
        }
    }

    fn pick(&self, params: &Value) -> Result<Value, PluginError> {
        let extensions: Vec<String> = serde_json::from_value(
            params
                .get("extensions")
                .cloned()
                .unwrap_or_else(|| json!([])),
        )
        .map_err(|_| error("INVALID_INPUT", "文件类型无效。"))?;
        if extensions.len() > 128
            || extensions.iter().any(|v| {
                v.is_empty() || v.len() > 32 || !v.bytes().all(|b| b.is_ascii_alphanumeric())
            })
        {
            return Err(error("INVALID_INPUT", "文件类型无效。"));
        }
        let limit = match params.get("maxBytes") {
            None => 128 * 1024 * 1024,
            Some(value) => value
                .as_u64()
                .filter(|v| *v > 0)
                .ok_or_else(|| error("INVALID_INPUT", "文件大小限制无效。"))?
                .min(128 * 1024 * 1024),
        };
        if self.files.lock().unwrap().len() >= 8 {
            return Err(error("RESOURCE_LIMIT", "一次调用最多选择 8 个文件。"));
        }
        let result = self.ask("pick", json!({"extensions":extensions,"maxBytes":limit}))?;
        if result.get("cancelled").and_then(Value::as_bool) == Some(true) {
            return Ok(json!({"cancelled":true}));
        }
        let handle = result
            .get("handle")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty() && v.len() <= 128)
            .ok_or_else(|| error("INVALID_INPUT", "文件标识无效。"))?;
        let name = result
            .get("name")
            .and_then(Value::as_str)
            .filter(|v| safe_selected_name(v))
            .ok_or_else(|| error("INVALID_INPUT", "文件名无效。"))?;
        let size = result
            .get("size")
            .and_then(Value::as_u64)
            .filter(|v| *v <= limit)
            .ok_or_else(|| error("RESOURCE_LIMIT", "文件超过允许的大小。"))?;
        let extension = name
            .rsplit_once('.')
            .map_or("", |(_, ext)| ext)
            .to_ascii_lowercase();
        if !extensions.is_empty()
            && !extensions
                .iter()
                .any(|v| v.eq_ignore_ascii_case(&extension))
        {
            return Err(error("INVALID_INPUT", "文件类型与插件要求不符。"));
        }
        let mime = crate::plugin_runtime::file_mime_type(&extension).to_owned();
        self.files.lock().unwrap().insert(
            handle.into(),
            SelectedFile {
                name: name.into(),
                size,
                mime: mime.clone(),
            },
        );
        Ok(json!({"cancelled":false,"handle":handle,"name":name,"size":size,"mimeType":mime}))
    }

    fn read(&self, params: &Value) -> Result<Value, PluginError> {
        let handle = params
            .get("handle")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let (name, size, mime) = self
            .files
            .lock()
            .unwrap()
            .get(handle)
            .map(|f| (f.name.clone(), f.size, f.mime.clone()))
            .ok_or_else(|| error("UNAUTHORIZED", "文件不属于当前远程调用。"))?;
        let chunked = params.get("offset").is_some() || params.get("chunkBytes").is_some();
        let (offset, count) = if chunked {
            let offset = params
                .get("offset")
                .and_then(Value::as_u64)
                .filter(|v| *v <= size)
                .ok_or_else(|| error("INVALID_INPUT", "文件偏移无效。"))?;
            let count = params
                .get("chunkBytes")
                .and_then(Value::as_u64)
                .filter(|v| (1..=CHUNK).contains(v))
                .ok_or_else(|| error("INVALID_INPUT", "文件分块大小无效。"))?;
            (offset, count.min(size - offset))
        } else {
            if size > 20 * 1024 * 1024 {
                return Err(error("RESOURCE_LIMIT", "较大文件需要插件分块读取。"));
            }
            (0, size)
        };
        let mut bytes = Vec::with_capacity(count as usize);
        while (bytes.len() as u64) < count {
            let length = CHUNK.min(count - bytes.len() as u64);
            let result = self.ask(
                "read",
                json!({"handle":handle,"offset":offset + bytes.len() as u64,"length":length}),
            )?;
            let encoded = result
                .get("contentBase64")
                .and_then(Value::as_str)
                .filter(|v| v.len() <= 1_398_104)
                .ok_or_else(|| error("INVALID_INPUT", "文件分块内容无效。"))?;
            let part = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|_| error("INVALID_INPUT", "文件内容编码无效。"))?;
            if part.len() as u64 != length {
                return Err(error("INVALID_INPUT", "文件分块长度不符。"));
            }
            bytes.extend_from_slice(&part);
        }
        let eof = offset + count == size;
        if eof {
            self.files.lock().unwrap().remove(handle);
        }
        let mut result = json!({"name":name,"mimeType":mime,"contentBase64":base64::engine::general_purpose::STANDARD.encode(bytes)});
        if chunked {
            result["offset"] = json!(offset);
            result["eof"] = json!(eof);
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    fn setup() -> (Arc<RemoteBroker>, Arc<RemoteContext>) {
        let broker = Arc::new(RemoteBroker::default());
        let context = Arc::new(RemoteContext::new(
            broker.clone(),
            "device-a".into(),
            "editor".into(),
        ));
        (broker, context)
    }
    fn next(broker: &RemoteBroker) -> Value {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let actions = broker.list("device-a");
            if let Some(action) = actions.as_array().unwrap().first() {
                return action.clone();
            }
            assert!(Instant::now() < deadline, "no interaction received");
            thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn exports_are_device_bound_and_never_write_to_the_host() {
        let (broker, context) = setup();
        let result = context
            .service(
                "core.files.export",
                &json!({"name":"result.csv","contentBase64":"YWJj"}),
            )
            .unwrap();
        assert_eq!(result["path"], "浏览器下载/result.csv");
        let action = next(&broker);
        let id = action["id"].as_str().unwrap();
        assert_eq!(broker.list("device-b"), json!([]));
        assert!(broker.download("device-b", id).is_err());
        assert!(broker.reply("device-b", id, Value::Null).is_err());
        assert_eq!(broker.download("device-a", id).unwrap().as_ref(), b"abc");
        context.cancel(); // Completed calls must leave downloadable files available.
        assert_eq!(broker.download("device-a", id).unwrap().as_ref(), b"abc");
        broker.close();
        assert!(broker.download("device-a", id).is_err());
        assert!(
            context
                .service("core.files.export", &json!({"name":"x","contentBase64":""}))
                .is_err()
        );
    }

    #[test]
    fn selected_bytes_use_validated_metadata_and_invocation_scoped_handles() {
        let (broker, context) = setup();
        let worker = context.clone();
        let task = thread::spawn(move || {
            worker.service(
                "core.files.pick",
                &json!({"extensions":["json"],"maxBytes":10}),
            )
        });
        let action = next(&broker);
        broker
            .reply(
                "device-a",
                action["id"].as_str().unwrap(),
                json!({"handle":"file-1","name":"配置.json","size":3,"mimeType":"ignored"}),
            )
            .unwrap();
        assert_eq!(
            task.join().unwrap().unwrap()["mimeType"],
            "application/json"
        );
        let other = context.for_plugin("other");
        assert!(
            other
                .service("core.files.read", &json!({"handle":"file-1"}))
                .is_err()
        );
        let worker = context.clone();
        let task =
            thread::spawn(move || worker.service("core.files.read", &json!({"handle":"file-1"})));
        let action = next(&broker);
        assert_eq!(action["payload"]["length"], 3);
        broker
            .reply(
                "device-a",
                action["id"].as_str().unwrap(),
                json!({"contentBase64":"YWJj"}),
            )
            .unwrap();
        let result = task.join().unwrap().unwrap();
        assert_eq!(result["contentBase64"], "YWJj");
        assert_eq!(result["name"], "配置.json");
        assert!(
            context
                .service("core.files.read", &json!({"handle":"file-1"}))
                .is_err()
        );
    }

    #[test]
    fn cancellation_unblocks_waiters_and_expiry_revokes_downloads() {
        let (broker, context) = setup();
        let worker = context.clone();
        let task = thread::spawn(move || worker.service("core.files.pick", &json!({})));
        let action = next(&broker);
        context.cancel();
        assert_eq!(task.join().unwrap().unwrap_err().code, "CANCELLED");
        assert!(
            broker
                .reply("device-a", action["id"].as_str().unwrap(), Value::Null)
                .is_err()
        );
        let context = RemoteContext::new(broker.clone(), "device-a".into(), "editor".into());
        context
            .service(
                "core.files.export",
                &json!({"name":"a.json","contentBase64":""}),
            )
            .unwrap();
        let action = next(&broker);
        broker
            .actions
            .lock()
            .unwrap()
            .get_mut(action["id"].as_str().unwrap())
            .unwrap()
            .expires = Instant::now();
        assert!(
            broker
                .download("device-a", action["id"].as_str().unwrap())
                .is_err()
        );
    }

    #[test]
    fn cancellation_propagates_downstream_but_not_to_parent_or_siblings() {
        let (broker, parent) = setup();
        let completed = parent.for_plugin("completed");
        completed.cancel();
        let child = parent.for_plugin("provider");
        let nested = child.for_plugin("nested-provider");
        let task = thread::spawn(move || nested.service("core.files.pick", &json!({})));
        assert_eq!(next(&broker)["pluginId"], "nested-provider");
        parent.cancel();
        assert_eq!(task.join().unwrap().unwrap_err().code, "CANCELLED");
        assert_eq!(broker.list("device-a"), json!([]));
        assert_eq!(
            parent
                .for_plugin("late-provider")
                .service("core.files.pick", &json!({}))
                .unwrap_err()
                .code,
            "CANCELLED"
        );
    }

    #[test]
    fn rejects_paths_host_directories_untrusted_urls_and_invalid_file_metadata() {
        let (broker, context) = setup();
        assert!(
            context
                .service(
                    "core.files.export",
                    &json!({"name":"../escape","contentBase64":""})
                )
                .is_err()
        );
        assert_eq!(
            context
                .service("core.files.reveal_own", &json!({"scope":"data"}))
                .unwrap_err()
                .code,
            "REMOTE_UNSUPPORTED"
        );
        assert!(
            context
                .service(
                    "core.browser.open_official",
                    &json!({"target":"document","pathId":"../escape"})
                )
                .is_err()
        );
        let worker = context.clone();
        let task = thread::spawn(move || {
            worker.service(
                "core.files.pick",
                &json!({"extensions":["json"],"maxBytes":10}),
            )
        });
        let action = next(&broker);
        broker
            .reply(
                "device-a",
                action["id"].as_str().unwrap(),
                json!({"handle":"file-1","name":"config.json","size":11}),
            )
            .unwrap();
        assert_eq!(task.join().unwrap().unwrap_err().code, "RESOURCE_LIMIT");
        assert!(
            context
                .service("core.files.read", &json!({"handle":"file-1"}))
                .is_err()
        );
    }
}
