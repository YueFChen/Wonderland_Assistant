//! 凭据加解密：Windows DPAPI 的最小公共封装。
//!
//! 工作区共用的 `seal` / `unseal` 两个入口，密码学由系统提供：
//! 密文与**当前 Windows 用户**绑定，换个用户或换台机器都解不开。
//!
//! 直接调用系统 API 而不引入额外依赖。非 Windows 平台明确返回 `Err`，
//! **不静默降级为明文落盘**。

#[cfg(windows)]
mod platform {
    use std::ffi::c_void;
    use std::ptr;

    /// Win32 `DATA_BLOB`。
    #[repr(C)]
    struct DataBlob {
        cb_data: u32,
        pb_data: *mut u8,
    }

    #[link(name = "crypt32")]
    unsafe extern "system" {
        fn CryptProtectData(
            p_data_in: *const DataBlob,
            sz_data_descr: *const u16,
            p_optional_entropy: *const DataBlob,
            pv_reserved: *mut c_void,
            p_prompt_struct: *mut c_void,
            dw_flags: u32,
            p_data_out: *mut DataBlob,
        ) -> i32;

        fn CryptUnprotectData(
            p_data_in: *const DataBlob,
            ppsz_data_descr: *mut *mut u16,
            p_optional_entropy: *const DataBlob,
            pv_reserved: *mut c_void,
            p_prompt_struct: *mut c_void,
            dw_flags: u32,
            p_data_out: *mut DataBlob,
        ) -> i32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LocalFree(h_mem: *mut c_void) -> *mut c_void;
    }

    /// 失败时不弹系统 UI，直接把错误交回调用方。
    const CRYPTPROTECT_UI_FORBIDDEN: u32 = 0x0000_0001;

    fn last_error() -> u32 {
        std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or_default() as u32
    }

    /// 取出系统分配的缓冲区内容并释放它。
    ///
    /// # Safety
    ///
    /// 调用方需保证 `blob.pb_data` 是系统分配且未被释放的有效缓冲区。
    unsafe fn take_blob(blob: &DataBlob) -> Vec<u8> {
        if blob.pb_data.is_null() || blob.cb_data == 0 {
            return Vec::new();
        }
        let bytes = unsafe { std::slice::from_raw_parts(blob.pb_data, blob.cb_data as usize) };
        let owned = bytes.to_vec();
        unsafe {
            LocalFree(blob.pb_data as *mut c_void);
        }
        owned
    }

    pub fn seal(plain: &[u8]) -> Result<Vec<u8>, String> {
        if plain.is_empty() {
            return Ok(Vec::new());
        }

        let input = DataBlob {
            cb_data: plain.len() as u32,
            pb_data: plain.as_ptr() as *mut u8,
        };
        let mut output = DataBlob {
            cb_data: 0,
            pb_data: ptr::null_mut(),
        };

        let ok = unsafe {
            CryptProtectData(
                &input,
                ptr::null(),
                ptr::null(),
                ptr::null_mut(),
                ptr::null_mut(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        };
        if ok == 0 {
            return Err(format!("CryptProtectData 失败（错误码 {}）", last_error()));
        }

        Ok(unsafe { take_blob(&output) })
    }

    pub fn unseal(sealed: &[u8]) -> Result<Vec<u8>, String> {
        if sealed.is_empty() {
            return Ok(Vec::new());
        }

        let input = DataBlob {
            cb_data: sealed.len() as u32,
            pb_data: sealed.as_ptr() as *mut u8,
        };
        let mut output = DataBlob {
            cb_data: 0,
            pb_data: ptr::null_mut(),
        };

        let ok = unsafe {
            CryptUnprotectData(
                &input,
                ptr::null_mut(),
                ptr::null(),
                ptr::null_mut(),
                ptr::null_mut(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        };
        if ok == 0 {
            return Err(format!(
                "CryptUnprotectData 失败（错误码 {}）",
                last_error()
            ));
        }

        Ok(unsafe { take_blob(&output) })
    }
}

#[cfg(not(windows))]
mod platform {
    pub fn seal(_plain: &[u8]) -> Result<Vec<u8>, String> {
        Err("凭据加密当前只实现了 Windows DPAPI".to_owned())
    }

    pub fn unseal(_sealed: &[u8]) -> Result<Vec<u8>, String> {
        Err("凭据加密当前只实现了 Windows DPAPI".to_owned())
    }
}

pub use platform::{seal, unseal};
