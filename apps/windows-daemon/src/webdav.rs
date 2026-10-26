use std::convert::Infallible;
use std::net::SocketAddr;
use std::time::{Duration, SystemTime};

use dav_server::DavHandler;
use dav_server::davpath::DavPath;
use dav_server::fs::*;
use futures_util::future::{BoxFuture, FutureExt};
use futures_util::stream::{BoxStream, StreamExt};
use hyper::{
    Server,
    service::{make_service_fn, service_fn},
};

use crate::phone::{MAX_READ, PhoneClient, PhoneError};
use crate::windroid::{ErrorCode, FileInfo};

#[derive(Clone, Debug)]
pub struct AndroidFs {
    phone: PhoneClient,
}

/// Run a blocking call to the phone without stalling the async executor
async fn blocking<T, F>(f: F) -> FsResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, PhoneError> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| FsError::GeneralFailure)?
        .map_err(fs_error)
}

fn fs_error(e: PhoneError) -> FsError {
    match e {
        PhoneError::Remote {
            code: ErrorCode::NotFound | ErrorCode::NotADirectory,
            ..
        } => FsError::NotFound,
        PhoneError::Remote {
            code: ErrorCode::AccessDenied,
            ..
        } => FsError::Forbidden,
        _ => FsError::GeneralFailure,
    }
}

fn android_path(path: &DavPath) -> String {
    let raw = String::from_utf8_lossy(path.as_bytes());
    let trimmed = raw.trim_end_matches('/');
    if trimmed.is_empty() {
        "/".to_string()
    } else if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{trimmed}")
    }
}

impl DavFileSystem for AndroidFs {
    fn open<'a>(
        &'a self,
        path: &'a DavPath,
        options: OpenOptions,
    ) -> BoxFuture<'a, FsResult<Box<dyn DavFile>>> {
        async move {
            // Read-only for now
            if options.write || options.append || options.create || options.create_new {
                return Err(FsError::Forbidden);
            }
            let p = android_path(path);
            let phone = self.phone.clone();
            let lookup = p.clone();
            let info = blocking(move || phone.stat(&lookup)).await?;
            Ok(Box::new(AndroidFile {
                phone: self.phone.clone(),
                path: p,
                info,
                offset: 0,
            }) as Box<dyn DavFile>)
        }
        .boxed()
    }

    fn read_dir<'a>(
        &'a self,
        path: &'a DavPath,
        _meta: ReadDirMeta,
    ) -> BoxFuture<'a, FsResult<BoxStream<'static, Box<dyn DavDirEntry>>>> {
        async move {
            let p = android_path(path);
            let phone = self.phone.clone();
            let files = blocking(move || phone.list_dir(&p)).await?;
            let entries: Vec<Box<dyn DavDirEntry>> = files
                .into_iter()
                .map(|file| Box::new(AndroidDirEntry { file }) as Box<dyn DavDirEntry>)
                .collect();
            Ok(futures_util::stream::iter(entries).boxed())
        }
        .boxed()
    }

    fn metadata<'a>(&'a self, path: &'a DavPath) -> BoxFuture<'a, FsResult<Box<dyn DavMetaData>>> {
        async move {
            let p = android_path(path);
            let phone = self.phone.clone();
            let info = blocking(move || phone.stat(&p)).await?;
            Ok(Box::new(AndroidMetaData::from(&info)) as Box<dyn DavMetaData>)
        }
        .boxed()
    }
}

#[derive(Clone, Debug)]
struct AndroidMetaData {
    size: u64,
    is_dir: bool,
    modified_ms: i64,
}

impl From<&FileInfo> for AndroidMetaData {
    fn from(info: &FileInfo) -> Self {
        AndroidMetaData {
            size: info.size_bytes.max(0) as u64,
            is_dir: info.is_directory,
            modified_ms: info.last_modified_ms,
        }
    }
}

impl DavMetaData for AndroidMetaData {
    fn len(&self) -> u64 {
        self.size
    }
    fn is_dir(&self) -> bool {
        self.is_dir
    }
    fn modified(&self) -> FsResult<SystemTime> {
        Ok(SystemTime::UNIX_EPOCH + Duration::from_millis(self.modified_ms.max(0) as u64))
    }
}

#[derive(Debug)]
struct AndroidDirEntry {
    file: FileInfo,
}

impl DavDirEntry for AndroidDirEntry {
    fn name(&self) -> Vec<u8> {
        self.file.name.as_bytes().to_vec()
    }
    fn metadata(&self) -> BoxFuture<'_, FsResult<Box<dyn DavMetaData>>> {
        async move { Ok(Box::new(AndroidMetaData::from(&self.file)) as Box<dyn DavMetaData>) }
            .boxed()
    }
}

#[derive(Debug)]
struct AndroidFile {
    phone: PhoneClient,
    path: String,
    info: FileInfo,
    offset: u64,
}

impl DavFile for AndroidFile {
    fn read_bytes(&mut self, count: usize) -> BoxFuture<'_, FsResult<bytes::Bytes>> {
        let phone = self.phone.clone();
        let path = self.path.clone();
        let start = self.offset;
        async move {
            // A single request is capped at MAX_READ, so larger reads are chunked
            let data = blocking(move || {
                let mut out = Vec::with_capacity(count);
                while out.len() < count {
                    let want = (count - out.len()).min(MAX_READ as usize) as u32;
                    let chunk = phone.read(&path, start + out.len() as u64, want)?;
                    let got = chunk.data.len();
                    out.extend_from_slice(&chunk.data);
                    if chunk.eof || got == 0 {
                        break;
                    }
                }
                Ok(out)
            })
            .await?;
            self.offset += data.len() as u64;
            Ok(bytes::Bytes::from(data))
        }
        .boxed()
    }

    fn metadata(&mut self) -> BoxFuture<'_, FsResult<Box<dyn DavMetaData>>> {
        let meta = AndroidMetaData::from(&self.info);
        async move { Ok(Box::new(meta) as Box<dyn DavMetaData>) }.boxed()
    }

    fn write_buf(&mut self, _buf: Box<dyn bytes::Buf + Send>) -> BoxFuture<'_, FsResult<()>> {
        async move { Err(FsError::Forbidden) }.boxed()
    }

    fn write_bytes(&mut self, _buf: bytes::Bytes) -> BoxFuture<'_, FsResult<()>> {
        async move { Err(FsError::Forbidden) }.boxed()
    }

    fn seek(&mut self, pos: std::io::SeekFrom) -> BoxFuture<'_, FsResult<u64>> {
        let size = self.info.size_bytes.max(0);
        let target = match pos {
            std::io::SeekFrom::Start(off) => Some(off as i64),
            std::io::SeekFrom::Current(diff) => (self.offset as i64).checked_add(diff),
            std::io::SeekFrom::End(diff) => size.checked_add(diff),
        };
        let result = match target {
            Some(t) if t >= 0 => {
                self.offset = t as u64;
                Ok(self.offset)
            }
            _ => Err(FsError::GeneralFailure),
        };
        async move { result }.boxed()
    }

    fn flush(&mut self) -> BoxFuture<'_, FsResult<()>> {
        async move { Ok(()) }.boxed()
    }
}

pub async fn serve_webdav(phone: PhoneClient, addr: SocketAddr) {
    let dav_server = DavHandler::builder()
        .filesystem(Box::new(AndroidFs { phone }))
        .build_handler();

    let make_service = make_service_fn(move |_| {
        let dav_server = dav_server.clone();
        async move {
            let func = move |req| {
                let dav_server = dav_server.clone();
                async move { Ok::<_, Infallible>(dav_server.handle(req).await) }
            };
            Ok::<_, Infallible>(service_fn(func))
        }
    });

    if let Err(e) = Server::bind(&addr).serve(make_service).await {
        log::warn!("WebDAV server error: {}", e);
    }
}
