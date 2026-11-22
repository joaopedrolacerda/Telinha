//! Quick Tunnel do Cloudflare (`*.trycloudflare.com`): não precisa de conta.
//! O link existe só enquanto o processo `cloudflared` estiver rodando.
use anyhow::{anyhow, Context, Result};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

pub struct Tunnel {
    child: Child,
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn resolve_binary(bin: &str) -> String {
    if bin != "cloudflared" {
        return bin.to_string();
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("cloudflared.exe"));
        }
    }
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        candidates.push(PathBuf::from(local).join("Microsoft\\WinGet\\Links\\cloudflared.exe"));
    }
    candidates.push(PathBuf::from("C:\\Program Files (x86)\\cloudflared\\cloudflared.exe"));
    candidates.push(PathBuf::from("C:\\Program Files\\cloudflared\\cloudflared.exe"));

    candidates
        .into_iter()
        .find(|p| p.exists())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| bin.to_string())
}

/// Coloca o processo num Job Object com "matar ao fechar": se o telinha fechar,
/// travar ou for morto, o Windows encerra o cloudflared junto e o link cai.
fn kill_with_parent(child: &Child) {
    use std::os::windows::io::AsRawHandle;
    use std::sync::OnceLock;
    use windows_sys::Win32::System::JobObjects::*;

    struct Job(isize);
    static JOB: OnceLock<Job> = OnceLock::new();

    let job = JOB.get_or_init(|| unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const _,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        Job(job as isize) // nunca fechado: só some quando o processo do telinha acaba
    });
    unsafe {
        AssignProcessToJobObject(job.0 as _, child.as_raw_handle() as _);
    }
}

pub fn start(bin: &str, port: u16) -> Result<(Tunnel, String)> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let bin = resolve_binary(bin);
    let mut child = Command::new(&bin)
        .args([
            "tunnel",
            "--no-autoupdate",
            "--url",
            &format!("http://127.0.0.1:{port}"),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW) // sem janela preta do cloudflared
        .spawn()
        .with_context(|| {
            format!(
                "não consegui rodar '{bin}'. Instale com: winget install Cloudflare.cloudflared \
                 (ou passe --cloudflared <caminho>, ou use --no-tunnel)"
            )
        })?;

    kill_with_parent(&child);
    let stderr = child.stderr.take().expect("stderr piped");
    let (tx, rx) = mpsc::channel::<String>();

    // Continua lendo o stderr para sempre, senão o pipe enche e o cloudflared trava.
    thread::spawn(move || {
        let mut sent = false;
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if !sent {
                if let Some(url) = find_url(&line) {
                    let _ = tx.send(url);
                    sent = true;
                }
            }
        }
    });

    let tunnel = Tunnel { child };
    let url = rx
        .recv_timeout(Duration::from_secs(45))
        .map_err(|_| anyhow!("o cloudflared não devolveu um link em 45s"))?;
    Ok((tunnel, url))
}

fn find_url(line: &str) -> Option<String> {
    let start = line.find("https://")?;
    let url: String = line[start..]
        .chars()
        .take_while(|c| !c.is_whitespace() && *c != '|')
        .collect();
    url.contains(".trycloudflare.com").then_some(url)
}

#[cfg(test)]
mod tests {
    use super::find_url;

    #[test]
    fn parses_quick_tunnel_banner() {
        let line = "2026-11-21T10:00:00Z INF |  https://foo-bar-baz.trycloudflare.com                                  |";
        assert_eq!(find_url(line).as_deref(), Some("https://foo-bar-baz.trycloudflare.com"));
        assert_eq!(find_url("INF see https://www.cloudflare.com/website-terms/"), None);
    }
}
