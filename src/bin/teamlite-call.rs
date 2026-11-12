//! Teamlite's call window: Teams on the web in an embedded Chromium (CEF) window, so calls,
//! meetings, video and screen sharing work without a separate browser.
//!
//! It is its own binary so the chat client never links libcef: Teamlite starts
//! `teamlite-call --url=<deep link>` only when a call or meeting is opened. CEF re-runs this same
//! binary for its renderer/GPU/utility processes (`--type=…`).
//!
//! One instance owns the profile at `~/.local/share/teamlite/calls` (Microsoft sign-in, cookies,
//! device choices). Launching again while it runs hands the URL to the running instance, which
//! opens it in a new window.

use std::cell::RefCell;
use std::sync::atomic::{AtomicUsize, Ordering};

use cef::*;

const DEFAULT_URL: &str = "https://teams.microsoft.com/v2/";
const APP_ID: &str = "teamlite";

/// Open windows; the process exits when the last one closes.
static BROWSERS: AtomicUsize = AtomicUsize::new(0);

fn main() {
    let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);
    let args = args::Args::new();
    let Some(cmd_line) = args.as_cmd_line() else {
        eprintln!("teamlite-call: couldn't read the command line");
        std::process::exit(1);
    };

    // Renderer, GPU and utility processes stop here.
    let code = execute_process(Some(args.as_main_args()), None::<&mut App>, std::ptr::null_mut());
    if code >= 0 {
        std::process::exit(code);
    }

    let profile = dirs::data_local_dir().unwrap_or_else(|| "/tmp".into()).join("teamlite/calls");
    let _ = std::fs::create_dir_all(&profile);
    if !instance_running(&profile) {
        // Chromium restores the windows of a session that didn't exit cleanly; a call window
        // should only ever show what was asked for.
        let _ = std::fs::remove_dir_all(profile.join("Default/Sessions"));
    }
    let profile_str = profile.to_string_lossy().to_string();
    let settings = Settings {
        no_sandbox: 1,
        root_cache_path: CefString::from(profile_str.as_str()),
        cache_path: CefString::from(profile_str.as_str()),
        persist_session_cookies: 1,
        log_severity: LogSeverity::ERROR,
        log_file: CefString::from(profile.join("cef.log").to_string_lossy().as_ref()),
        ..Default::default()
    };

    let mut app = CallApp::new(target_url(&cmd_line));
    if initialize(Some(args.as_main_args()), Some(&settings), Some(&mut app), std::ptr::null_mut()) != 1 {
        // With the Chrome runtime a second launch on the same profile forwards its command line to
        // the running instance (see `on_already_running_app_relaunch`) and fails here: that's success.
        return;
    }
    run_message_loop();
    shutdown();
}

/// Whether another call window owns the profile (Chromium's `SingletonLock` → `host-pid`).
fn instance_running(profile: &std::path::Path) -> bool {
    let Ok(target) = std::fs::read_link(profile.join("SingletonLock")) else { return false };
    let target = target.to_string_lossy().to_string();
    let Some(pid) = target.rsplit('-').next().and_then(|p| p.parse::<u32>().ok()) else { return false };
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}

/// `--url=…` (or the first plain argument), else the Teams home page.
fn target_url(cmd: &CommandLine) -> String {
    let url = CefString::from(&cmd.switch_value(Some(&CefString::from("url")))).to_string();
    let url = if url.is_empty() { std::env::args().skip(1).find(|a| !a.starts_with("--")).unwrap_or_default() } else { url };
    if is_teams_url(&url) { url } else { DEFAULT_URL.to_string() }
}

fn host_of(url: &str) -> String {
    url.split_once("://").map(|(_, rest)| rest).unwrap_or("").split(['/', '?', '#', ':']).next().unwrap_or("").to_ascii_lowercase()
}

/// Origins that get camera, microphone, screen and notification access without a prompt.
fn is_teams_host(host: &str) -> bool {
    ["teams.microsoft.com", "teams.cloud.microsoft", "teams.live.com", "teams.microsoft.us"]
        .iter()
        .any(|d| host == *d || host.ends_with(&format!(".{d}")))
}

fn is_teams_url(url: &str) -> bool {
    url.starts_with("https://") && is_teams_host(&host_of(url))
}

/// Opens `url` in a new top-level window (UI thread only).
fn open_window(url: &str) {
    let mut client = CallClient::new();
    let mut view_delegate = CallBrowserViewDelegate::new();
    let browser_view = browser_view_create(
        Some(&mut client),
        Some(&CefString::from(url)),
        Some(&BrowserSettings::default()),
        None,
        None,
        Some(&mut view_delegate),
    );
    let mut window_delegate = CallWindowDelegate::new(RefCell::new(browser_view));
    window_create_top_level(Some(&mut window_delegate));
}

wrap_app! {
    struct CallApp {
        url: String,
    }

    impl App {
        fn on_before_command_line_processing(&self, process_type: Option<&CefString>, command_line: Option<&mut CommandLine>) {
            // Browser process only; children inherit what they need.
            if process_type.is_some_and(|t| !t.to_string().is_empty()) {
                return;
            }
            let Some(cmd) = command_line else { return };
            // Screen sharing through the desktop portal on Wayland, and no keyring prompt for
            // the profile's cookie encryption.
            cmd.append_switch_with_value(Some(&CefString::from("enable-features")), Some(&CefString::from("WebRTCPipeWireCapturer")));
            cmd.append_switch_with_value(Some(&CefString::from("password-store")), Some(&CefString::from("basic")));
            cmd.append_switch(Some(&CefString::from("disable-session-crashed-bubble")));
        }

        fn browser_process_handler(&self) -> Option<BrowserProcessHandler> {
            Some(CallProcessHandler::new(self.url.clone()))
        }
    }
}

wrap_browser_process_handler! {
    struct CallProcessHandler {
        url: String,
    }

    impl BrowserProcessHandler {
        fn on_context_initialized(&self) {
            open_window(&self.url);
        }

        fn on_already_running_app_relaunch(&self, command_line: Option<&mut CommandLine>, _current_directory: Option<&CefString>) -> i32 {
            let url = command_line.map(|c| target_url(c)).unwrap_or_else(|| DEFAULT_URL.to_string());
            open_window(&url);
            1
        }
    }
}

wrap_window_delegate! {
    struct CallWindowDelegate {
        browser_view: RefCell<Option<BrowserView>>,
    }

    impl ViewDelegate {
        fn preferred_size(&self, _view: Option<&mut View>) -> Size {
            Size { width: 1180, height: 780 }
        }
    }

    impl PanelDelegate {}

    impl WindowDelegate {
        fn on_window_created(&self, window: Option<&mut Window>) {
            let browser_view = self.browser_view.borrow();
            let (Some(window), Some(browser_view)) = (window, browser_view.as_ref()) else { return };
            let mut view = View::from(browser_view);
            window.add_child_view(Some(&mut view));
            window.set_title(Some(&CefString::from("Teams · Teamlite")));
            window.show();
            browser_view.request_focus();
        }

        fn on_window_destroyed(&self, _window: Option<&mut Window>) {
            *self.browser_view.borrow_mut() = None;
        }

        fn can_close(&self, _window: Option<&mut Window>) -> i32 {
            let browser_view = self.browser_view.borrow();
            match browser_view.as_ref().and_then(|v| v.browser()).and_then(|b| b.host()) {
                Some(host) => host.try_close_browser(),
                None => 1,
            }
        }

        fn linux_window_properties(&self, _window: Option<&mut Window>, properties: Option<&mut LinuxWindowProperties>) -> i32 {
            // Group call windows with Teamlite in docks and task switchers.
            let Some(p) = properties else { return 0 };
            p.wayland_app_id = CefString::from(APP_ID);
            p.wm_class_class = CefString::from(APP_ID);
            p.wm_class_name = CefString::from(APP_ID);
            1
        }
    }
}

wrap_browser_view_delegate! {
    struct CallBrowserViewDelegate {}

    impl ViewDelegate {}

    impl BrowserViewDelegate {
        fn on_popup_browser_view_created(&self, _browser_view: Option<&mut BrowserView>, popup_browser_view: Option<&mut BrowserView>, _is_devtools: i32) -> i32 {
            // Sign-in and "pop out" windows get a plain window of their own.
            let mut delegate = CallWindowDelegate::new(RefCell::new(popup_browser_view.cloned()));
            window_create_top_level(Some(&mut delegate));
            1
        }

        fn chrome_toolbar_type(&self, _browser_view: Option<&mut BrowserView>) -> ChromeToolbarType {
            ChromeToolbarType::NONE
        }
    }
}

wrap_client! {
    struct CallClient {}

    impl Client {
        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(CallLifeSpan::new())
        }

        fn display_handler(&self) -> Option<DisplayHandler> {
            Some(CallDisplay::new())
        }

        fn permission_handler(&self) -> Option<PermissionHandler> {
            Some(CallPermissions::new())
        }

        fn request_handler(&self) -> Option<RequestHandler> {
            Some(CallRequests::new())
        }

        fn load_handler(&self) -> Option<LoadHandler> {
            Some(CallLoad::new())
        }
    }
}

wrap_life_span_handler! {
    struct CallLifeSpan {}

    impl LifeSpanHandler {
        fn on_after_created(&self, _browser: Option<&mut Browser>) {
            BROWSERS.fetch_add(1, Ordering::SeqCst);
        }

        fn on_before_close(&self, _browser: Option<&mut Browser>) {
            if BROWSERS.fetch_sub(1, Ordering::SeqCst) <= 1 {
                quit_message_loop();
            }
        }
    }
}

wrap_display_handler! {
    struct CallDisplay {}

    impl DisplayHandler {
        fn on_title_change(&self, browser: Option<&mut Browser>, title: Option<&CefString>) {
            let mut browser = browser.cloned();
            let Some(window) = browser_view_get_for_browser(browser.as_mut()).and_then(|v| v.window()) else { return };
            let title = title.map(|t| t.to_string()).filter(|t| !t.trim().is_empty()).unwrap_or_else(|| "Teams".into());
            window.set_title(Some(&CefString::from(format!("{title} · Teamlite").as_str())));
        }
    }
}

wrap_permission_handler! {
    struct CallPermissions {}

    impl PermissionHandler {
        fn on_request_media_access_permission(&self, _browser: Option<&mut Browser>, _frame: Option<&mut Frame>, requesting_origin: Option<&CefString>, requested_permissions: u32, callback: Option<&mut MediaAccessCallback>) -> i32 {
            let origin = requesting_origin.map(|o| o.to_string()).unwrap_or_default();
            if !is_teams_host(&host_of(&origin)) {
                return 0; // Chromium's own prompt
            }
            let Some(cb) = callback else { return 0 };
            cb.cont(requested_permissions);
            1
        }

        fn on_show_permission_prompt(&self, _browser: Option<&mut Browser>, _prompt_id: u64, requesting_origin: Option<&CefString>, _requested_permissions: u32, callback: Option<&mut PermissionPromptCallback>) -> i32 {
            let origin = requesting_origin.map(|o| o.to_string()).unwrap_or_default();
            if !is_teams_host(&host_of(&origin)) {
                return 0;
            }
            let Some(cb) = callback else { return 0 };
            cb.cont(PermissionRequestResult::ACCEPT);
            1
        }
    }
}

wrap_request_handler! {
    struct CallRequests {}

    impl RequestHandler {
        fn on_before_browse(&self, _browser: Option<&mut Browser>, _frame: Option<&mut Frame>, request: Option<&mut Request>, _user_gesture: i32, _is_redirect: i32) -> i32 {
            // The deep-link launcher tries `msteams:` to start the desktop app; we are the app.
            let url = request.map(|r| CefString::from(&r.url()).to_string()).unwrap_or_default();
            let scheme = url.split(':').next().unwrap_or("").to_ascii_lowercase();
            i32::from(scheme.starts_with("msteams") || scheme == "ms-teams")
        }
    }
}

/// On the deep-link launcher page ("How do you want to open this?"), pick the web app.
const CONTINUE_ON_WEB_JS: &str = r#"
(() => {
  const labels = [/continue on this browser/i, /use the web app instead/i, /join on the web instead/i];
  let tries = 0;
  const t = setInterval(() => {
    tries++;
    const el = document.querySelector('[data-tid="joinOnWeb"], #openTeamsClientInBrowser')
      || [...document.querySelectorAll('button, a')].find(b => labels.some(r => r.test(b.textContent || '')));
    if (el) { clearInterval(t); el.click(); }
    if (tries > 40) clearInterval(t);
  }, 250);
})();
"#;

wrap_load_handler! {
    struct CallLoad {}

    impl LoadHandler {
        fn on_load_end(&self, _browser: Option<&mut Browser>, frame: Option<&mut Frame>, _http_status_code: i32) {
            let Some(frame) = frame else { return };
            if frame.is_main() == 0 {
                return;
            }
            let url = CefString::from(&frame.url()).to_string();
            if is_teams_url(&url) && (url.contains("/l/") || url.contains("/dl/launcher")) {
                frame.execute_java_script(Some(&CefString::from(CONTINUE_ON_WEB_JS)), Some(&CefString::from(url.as_str())), 0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn teams_hosts() {
        assert!(is_teams_url("https://teams.microsoft.com/l/call/0/0?users=a%40b.c"));
        assert!(is_teams_url("https://teams.cloud.microsoft/v2/"));
        assert!(!is_teams_url("http://teams.microsoft.com/"));
        assert!(!is_teams_url("https://teams.microsoft.com.evil.example/"));
        assert!(!is_teams_url("https://evilteams.microsoft.com.example/"));
        assert_eq!(host_of("https://Teams.Microsoft.com:443/x"), "teams.microsoft.com");
    }
}
