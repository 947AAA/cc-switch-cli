//! Optional reverse tunnel for `cc-switch web serve --tunnel`.
//!
//! Shells out to the locally-installed `tailscale` CLI to expose the loopback
//! web server to either the private tailnet (`tailscale serve`, the safe
//! default) or the public internet (`tailscale funnel`, opt-in via
//! `--tunnel-public`). We use `--bg`, so tailscaled owns the proxying and there
//! is no child process to babysit — just a setup call and an `off` teardown,
//! which runs from [`Tunnel`]'s `Drop` so it always fires.
//!
//! The web server still binds 127.0.0.1 only; the tailscale proxy connects to
//! it locally, so the loopback-only + session-token design is unchanged.
//!
//! Beginner-friendly: [`ensure_ready`] walks the user through install and login
//! (running `tailscale up` for them) instead of failing with a terse error.
//!
//! Overrides for rootless / userspace daemons (no root, custom socket):
//!   - `CC_SWITCH_TAILSCALE`         path to the tailscale binary (default: PATH)
//!   - `CC_SWITCH_TAILSCALE_SOCKET`  tailscaled socket, passed as `--socket`

use std::path::Path;
use std::process::Command;

use crate::AppError;

/// Tailnet-side HTTPS port. 8443 (not 443) by default so we never clobber an
/// existing `tailscale serve`/`funnel` mapping on the standard HTTPS port — and
/// so our `off` teardown can't remove someone else's service.
const HTTPS_PORT: u16 = 8443;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TunnelMode {
    /// Private: reachable only by devices on your tailnet (`tailscale serve`).
    Serve,
    /// Public: reachable by anyone with the URL (`tailscale funnel`).
    Funnel,
}

impl TunnelMode {
    fn subcommand(self) -> &'static str {
        match self {
            TunnelMode::Serve => "serve",
            TunnelMode::Funnel => "funnel",
        }
    }
}

/// An active tailscale serve/funnel mapping. Tears itself down on drop.
pub struct Tunnel {
    mode: TunnelMode,
    target_port: u16,
}

impl Tunnel {
    /// Set up `tailscale {serve|funnel} --bg` pointing at `localhost:target_port`.
    /// Returns the tunnel handle and the base URL (no token).
    pub fn start(mode: TunnelMode, target_port: u16) -> Result<(Self, String), AppError> {
        ensure_ready()?;

        let target = format!("localhost:{target_port}");
        let status = tailscale()
            .args([
                mode.subcommand(),
                "--bg",
                &format!("--https={HTTPS_PORT}"),
                "--yes",
                &target,
            ])
            .output()
            .map_err(|e| AppError::Message(format!("failed to run `tailscale`: {e}")))?;

        if !status.status.success() {
            let stderr = String::from_utf8_lossy(&status.stderr);
            return Err(AppError::Message(format!(
                "`tailscale {}` failed: {}",
                mode.subcommand(),
                stderr.trim()
            )));
        }

        let host = dns_name()?;
        let url = format!("https://{host}:{HTTPS_PORT}");
        Ok((Self { mode, target_port }, url))
    }

    fn teardown(&self) {
        let target = format!("localhost:{}", self.target_port);
        let _ = tailscale()
            .args([
                self.mode.subcommand(),
                &format!("--https={HTTPS_PORT}"),
                "--yes",
                &target,
                "off",
            ])
            .output();
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        self.teardown();
    }
}

/// Build a `tailscale` Command, honoring the binary-path / socket env overrides.
fn tailscale() -> Command {
    let bin = std::env::var("CC_SWITCH_TAILSCALE").unwrap_or_else(|_| "tailscale".to_string());
    let mut cmd = Command::new(bin);
    if let Ok(sock) = std::env::var("CC_SWITCH_TAILSCALE_SOCKET") {
        if !sock.is_empty() {
            cmd.arg(format!("--socket={sock}"));
        }
    }
    cmd
}

fn installed() -> bool {
    if let Ok(bin) = std::env::var("CC_SWITCH_TAILSCALE") {
        return Path::new(&bin).exists();
    }
    which::which("tailscale").is_ok()
}

/// OS-specific, copy-pasteable install guidance for a first-time user.
fn install_hint() -> &'static str {
    match std::env::consts::OS {
        "macos" => crate::t!(
            "Install Tailscale from the Mac App Store or https://tailscale.com/download, open the app and sign in, then retry.",
            "从 Mac App Store 或 https://tailscale.com/download 安装 Tailscale，打开 App 登录后重试。"
        ),
        "windows" => crate::t!(
            "Install Tailscale from https://tailscale.com/download, sign in, then retry.",
            "从 https://tailscale.com/download 安装 Tailscale，登录后重试。"
        ),
        _ => crate::t!(
            "Install Tailscale (rootless is fine), then retry:\n  curl -fsSL https://tailscale.com/install.sh | sh",
            "安装 Tailscale（无需 root 也可），然后重试：\n  curl -fsSL https://tailscale.com/install.sh | sh"
        ),
    }
}

/// The tailscale backend state (`Running`, `NeedsLogin`, `Stopped`, …). Errors
/// if the daemon/service isn't reachable.
fn backend_state() -> Result<String, AppError> {
    let out = tailscale()
        .args(["status", "--json"])
        .output()
        .map_err(|e| AppError::Message(format!("failed to run `tailscale`: {e}")))?;
    if !out.status.success() {
        return Err(AppError::Message(crate::t!(
            "Tailscale is installed but its service isn't running. Start the Tailscale app (macOS/Windows) or the tailscaled service (Linux), then retry.",
            "Tailscale 已安装但服务未运行。请启动 Tailscale 应用（macOS/Windows）或 tailscaled 服务（Linux）后重试。"
        ).into()));
    }
    let json: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| AppError::Message(format!("invalid `tailscale status` JSON: {e}")))?;
    Ok(json
        .get("BackendState")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string())
}

/// Beginner-friendly readiness check: install present + logged in. If the node
/// isn't logged in, run `tailscale up` interactively (it prints a login URL the
/// user opens in a browser) and wait for it to complete.
fn ensure_ready() -> Result<(), AppError> {
    if !installed() {
        return Err(AppError::Message(format!(
            "{}\n{}",
            crate::t!("Tailscale is not installed.", "未检测到 Tailscale。"),
            install_hint()
        )));
    }

    if backend_state()? != "Running" {
        println!(
            "{}",
            crate::cli::ui::info(crate::t!(
                "Tailscale needs to log in — a browser login URL will appear below:",
                "Tailscale 需要登录——下面会出现一个浏览器登录链接："
            ))
        );
        // Inherit stdio so the user sees tailscale's own "To authenticate,
        // visit: <URL>" prompt and the call blocks until they finish.
        let status = tailscale()
            .arg("up")
            .status()
            .map_err(|e| AppError::Message(format!("failed to run `tailscale up`: {e}")))?;
        if !status.success() {
            return Err(AppError::Message(
                crate::t!(
                    "Tailscale login did not complete. Run `tailscale up` manually, then retry.",
                    "Tailscale 登录未完成。请手动运行 `tailscale up` 后重试。"
                )
                .into(),
            ));
        }
    }
    Ok(())
}

/// The node's MagicDNS name (e.g. `machine.tailnet.ts.net`), trailing dot stripped.
fn dns_name() -> Result<String, AppError> {
    let out = tailscale()
        .args(["status", "--json"])
        .output()
        .map_err(|e| AppError::Message(format!("failed to run `tailscale status --json`: {e}")))?;
    if !out.status.success() {
        return Err(AppError::Message("failed to read tailscale status".into()));
    }
    let json: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| AppError::Message(format!("invalid `tailscale status` JSON: {e}")))?;
    let dns = json
        .get("Self")
        .and_then(|s| s.get("DNSName"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AppError::Message("`tailscale status` did not report this node's DNS name".into())
        })?;
    Ok(dns.trim_end_matches('.').to_string())
}
