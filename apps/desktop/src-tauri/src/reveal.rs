//! 使用系统文件管理器或默认浏览器打开目标。

use std::path::Path;
use wonderland_kernel::KernelError;

/// 用系统文件管理器打开目录；目录还不存在时给出可读的提示。
///
/// 目录不存在时返回错误。
pub fn open_dir(path: &Path) -> Result<(), KernelError> {
    if !path.is_dir() {
        return Err(KernelError::Other(format!(
            "导出目录还不存在：{}",
            path.display()
        )));
    }
    platform::open(path).map_err(|error| KernelError::Other(format!("打开导出目录失败：{error}")))
}

/// Open a URL through the platform browser. Callers must validate the destination before using it.
pub fn open_url(url: &str) -> Result<(), KernelError> {
    platform::open_url(url).map_err(|error| KernelError::Other(format!("打开链接失败：{error}")))
}

#[cfg(windows)]
mod platform {
    use std::{io, path::Path, process::Command};

    pub(super) fn open(path: &Path) -> io::Result<()> {
        Command::new("explorer").arg(path).spawn().map(|_| ())
    }

    pub(super) fn open_url(url: &str) -> io::Result<()> {
        Command::new("rundll32.exe")
            .args(["url.dll,FileProtocolHandler", url])
            .spawn()
            .map(|_| ())
    }
}

#[cfg(not(windows))]
mod platform {
    use std::{io, path::Path};

    pub(super) fn open(_path: &Path) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "打开文件位置当前只实现了 Windows",
        ))
    }

    pub(super) fn open_url(_url: &str) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "打开链接当前只实现了 Windows"))
    }
}
