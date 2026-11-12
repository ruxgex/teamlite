//! Interactive sign-in. The Microsoft login page runs in a small WebKitGTK window in a child
//! process (`teamlite --login-webview <url>`), so GTK's loop never meets the app's winit loop.
//! The child prints the redirect URL carrying the authorization code and exits.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use crate::api::auth::{self, Identity, Pkce, Tokens};

/// Blocks until the user finishes signing in (call off the UI thread).
pub fn interactive(login_hint: Option<&str>) -> Result<(Tokens, Identity)> {
    interactive_in("organizations", login_hint)
}

/// Sign-in against one tenant's authority, so its own policies (MFA…) apply.
pub fn interactive_in(tenant: &str, login_hint: Option<&str>) -> Result<(Tokens, Identity)> {
    let pkce = Pkce::new();
    let url = auth::authorize_url_in(tenant, &pkce, login_hint);
    let redirect = if cfg!(feature = "webview-login") {
        let exe = std::env::current_exe().context("current exe")?;
        let mut child = Command::new(exe)
            .arg("--login-webview")
            .arg(&url)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("couldn't open the sign-in window")?;
        let mut line = String::new();
        if let Some(out) = child.stdout.take() {
            BufReader::new(out).read_line(&mut line).ok();
        }
        let _ = child.wait();
        if line.trim().is_empty() {
            bail!("Sign-in window closed");
        }
        line
    } else {
        eprintln!("Open this URL, sign in, then paste the final URL (it starts with {}):\n{url}", auth::redirect_uri());
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        line
    };
    let code = auth::code_from_redirect(&redirect)?;
    auth::redeem_code_in(tenant, &code, &pkce.verifier)
}

/// The child process: a browser window that stops at the native-client redirect.
#[cfg(feature = "webview-login")]
pub fn webview_main(url: &str) -> ! {
    use tao::dpi::LogicalSize;
    use tao::event::{Event, WindowEvent};
    use tao::event_loop::{ControlFlow, EventLoopBuilder};
    use tao::platform::unix::WindowExtUnix;
    use tao::window::WindowBuilder;
    use wry::{WebContext, WebViewBuilder, WebViewBuilderExtUnix};

    let event_loop = EventLoopBuilder::<String>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let window = WindowBuilder::new()
        .with_title("Sign in · Teamlite")
        .with_inner_size(LogicalSize::new(480.0, 700.0))
        .build(&event_loop)
        .expect("login window");
    // Keep the login cookies so "Stay signed in" and SSO work for later sign-ins.
    let data = dirs::data_local_dir().unwrap_or_else(|| "/tmp".into()).join("teamlite/webview");
    let mut context = WebContext::new(Some(data));
    let nav_proxy = proxy.clone();
    let builder = WebViewBuilder::new_with_web_context(&mut context)
        .with_url(url)
        .with_navigation_handler(move |u: String| {
            if u.starts_with(auth::redirect_uri()) {
                let _ = nav_proxy.send_event(u);
                false
            } else {
                true
            }
        });
    let vbox = window.default_vbox().expect("gtk vbox");
    let _webview = builder.build_gtk(vbox).expect("webview");
    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::Wait;
        match event {
            Event::UserEvent(u) => {
                println!("{u}");
                std::process::exit(0);
            }
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => std::process::exit(1),
            _ => {}
        }
    })
}
