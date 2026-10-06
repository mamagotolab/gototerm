//! One local, nonblocking message pipe per PTY. The reader owns the Win32 handle.
use crate::gt::GtMessage;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

type Handle = *mut c_void;
#[link(name = "kernel32")]
extern "system" {
    fn CreateNamedPipeW(
        name: *const u16,
        access: u32,
        mode: u32,
        instances: u32,
        out_size: u32,
        in_size: u32,
        timeout: u32,
        security: *const c_void,
    ) -> Handle;
    fn ConnectNamedPipe(pipe: Handle, overlapped: *mut c_void) -> i32;
    fn DisconnectNamedPipe(pipe: Handle) -> i32;
    fn ReadFile(
        file: Handle,
        buffer: *mut c_void,
        count: u32,
        read: *mut u32,
        overlapped: *mut c_void,
    ) -> i32;
    fn CloseHandle(handle: Handle) -> i32;
    fn GetLastError() -> u32;
}

pub(crate) struct StatePipe {
    pub name: String,
    stopped: Arc<AtomicBool>,
}

impl StatePipe {
    pub fn new(messages: Arc<Mutex<Vec<GtMessage>>>) -> Option<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_nanos();
        let name = format!(
            r"\\.\pipe\gototerm-state-{}-{id}-{stamp:x}",
            std::process::id()
        );
        let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        // INBOUND | FIRST_PIPE_INSTANCE; MESSAGE | READMODE_MESSAGE | NOWAIT |
        // REJECT_REMOTE_CLIENTS. The default security descriptor uses the owner's token.
        let handle = unsafe {
            CreateNamedPipeW(
                wide.as_ptr(),
                1 | 0x80000,
                4 | 2 | 1 | 8,
                1,
                0,
                256,
                0,
                std::ptr::null(),
            )
        };
        if handle as isize == -1 {
            log::warn!("native state pipe could not be created");
            return None;
        }
        let raw = handle as usize;
        let stopped = Arc::new(AtomicBool::new(false));
        let done = stopped.clone();
        std::thread::spawn(move || {
            let handle = raw as Handle;
            let mut connected = false;
            let mut since = std::time::Instant::now();
            while !done.load(Ordering::Relaxed) {
                if !connected {
                    let ok = unsafe { ConnectNamedPipe(handle, std::ptr::null_mut()) } != 0;
                    let error = if ok { 0 } else { unsafe { GetLastError() } };
                    // NOWAIT success merely makes a disconnected instance available.
                    // NO_DATA can mean the client wrote and closed before this poll;
                    // ReadFile must drain its buffered message before disconnection.
                    connected = matches!(error, 535 | 232);
                    if connected {
                        since = std::time::Instant::now();
                    }
                }
                if connected {
                    let mut buf = [0u8; 129];
                    let mut count = 0;
                    let ok = unsafe {
                        ReadFile(
                            handle,
                            buf.as_mut_ptr().cast(),
                            buf.len() as u32,
                            &mut count,
                            std::ptr::null_mut(),
                        )
                    } != 0;
                    let error = if ok { 0 } else { unsafe { GetLastError() } };
                    if ok && count > 0 {
                        if let Some(event) = crate::agent_hooks::parse_event(&buf[..count as usize])
                        {
                            let mut queue = messages.lock().unwrap();
                            if queue.len() < 1024 {
                                queue.push(event);
                            }
                        }
                    }
                    if (ok && count > 0)
                        || (error != 0 && error != 232)
                        || since.elapsed() > std::time::Duration::from_secs(1)
                    {
                        unsafe {
                            DisconnectNamedPipe(handle);
                        }
                        connected = false;
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            unsafe {
                DisconnectNamedPipe(handle);
                CloseHandle(handle);
            }
        });
        Some(Self { name, stopped })
    }
}

impl Drop for StatePipe {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    fn send(name: &str, bytes: &[u8]) {
        for _ in 0..100 {
            if let Ok(mut file) = std::fs::OpenOptions::new().write(true).open(name) {
                file.write_all(bytes).unwrap();
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("pipe did not accept the client");
    }
    #[test]
    fn separate_panes_route_closed_clients_and_reject_oversized_events() {
        let a = Arc::new(Mutex::new(Vec::new()));
        let b = Arc::new(Mutex::new(Vec::new()));
        let pipe_a = StatePipe::new(a.clone()).unwrap();
        let pipe_b = StatePipe::new(b.clone()).unwrap();
        assert_ne!(pipe_a.name, pipe_b.name);
        send(&pipe_a.name, br#"{"agent":"claude","state":"blocked"}"#);
        send(&pipe_b.name, br#"{"agent":"codex","state":"done"}"#);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while (a.lock().unwrap().is_empty() || b.lock().unwrap().is_empty())
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(matches!(
            a.lock().unwrap().as_slice(),
            [GtMessage::State {
                signal: crate::gt::AgentSignal::Blocked,
                ..
            }]
        ));
        assert!(matches!(
            b.lock().unwrap().as_slice(),
            [GtMessage::State {
                signal: crate::gt::AgentSignal::Done,
                ..
            }]
        ));
        send(&pipe_a.name, &[b'x'; 200]);
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(a.lock().unwrap().len(), 1);
        let name = pipe_a.name.clone();
        drop(pipe_a);
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(std::fs::OpenOptions::new().write(true).open(name).is_err());
    }
}
