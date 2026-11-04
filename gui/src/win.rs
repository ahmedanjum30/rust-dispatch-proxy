//! The window, built from Windows' own controls (buttons, checkboxes, edit
//! boxes and a list view), so proxy-gui.exe needs no GUI library and stays
//! small. Look and DPI scaling come from Windows; the embedded manifest
//! (build.rs) turns on modern control styles and per-monitor DPI awareness.

use std::cell::RefCell;
use std::process::Child;
use std::sync::atomic::{AtomicU16, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, COLOR_WINDOW, CreateFontW, DEFAULT_CHARSET, DeleteObject, FW_NORMAL, FW_SEMIBOLD, GetSysColor,
    GetSysColorBrush, HDC, HFONT, InvalidateRect, OUT_DEFAULT_PRECIS, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::{
    BST_CHECKED, BST_UNCHECKED, ICC_LISTVIEW_CLASSES, ICC_STANDARD_CLASSES, INITCOMMONCONTROLSEX, InitCommonControlsEx, LVCF_FMT, LVCF_TEXT,
    LVCF_WIDTH, LVCFMT_LEFT, LVCFMT_RIGHT, LVCOLUMNW, LVIF_TEXT, LVITEMW, LVM_DELETEALLITEMS, LVM_GETITEMCOUNT, LVM_INSERTCOLUMNW, LVM_INSERTITEMW,
    LVM_SETCOLUMNWIDTH, LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETITEMTEXTW, LVS_EX_DOUBLEBUFFER, LVS_EX_FULLROWSELECT, LVS_NOSORTHEADER, LVS_REPORT,
    LVS_SINGLESEL, WC_LISTVIEWW,
};
use windows_sys::Win32::UI::HiDpi::{GetDpiForSystem, GetDpiForWindow};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    BM_GETCHECK, BM_SETCHECK, BN_CLICKED, BS_AUTOCHECKBOX, BS_GROUPBOX, BS_PUSHBUTTON, CREATESTRUCTW, CW_USEDEFAULT, CreateWindowExW, DefWindowProcW,
    DispatchMessageW, EN_CHANGE, ES_AUTOHSCROLL, ES_NUMBER, GetClientRect, GetDlgItem, GetMessageW, GetWindowTextLengthW, GetWindowTextW, HMENU,
    IDC_ARROW, IDI_APPLICATION, IsDialogMessageW, IsWindowVisible, LoadCursorW, LoadIconW, MINMAXINFO, MSG, MoveWindow, PostMessageW,
    PostQuitMessage, RegisterClassExW, SW_HIDE, SW_SHOW, SWP_NOACTIVATE, SWP_NOZORDER, SendMessageW, SetWindowPos, SetWindowTextW, ShowWindow,
    TranslateMessage, WM_APP, WM_COMMAND, WM_CREATE, WM_CTLCOLORSTATIC, WM_DESTROY, WM_DPICHANGED, WM_GETMINMAXINFO, WM_SETFONT, WM_SIZE,
    WNDCLASSEXW, WS_CHILD, WS_CLIPCHILDREN, WS_EX_CLIENTEDGE, WS_OVERLAPPEDWINDOW, WS_TABSTOP, WS_VISIBLE,
};

use crate::control;
use crate::settings::{self, Settings};
use crate::status::{self, Reading};

/// Posted by the status poller after every reading.
const WM_STATUS: u32 = WM_APP + 1;
/// Posted when the "allow Windows apps" job has finished.
const WM_APPS_DONE: u32 = WM_APP + 2;

// Control IDs.
const ID_TITLE: i32 = 100;
const ID_STATE: i32 = 101;
const ID_DETAILS: i32 = 102;
const ID_START: i32 = 110;
const ID_STOP: i32 = 111;
const ID_RESTART: i32 = 112;
const ID_STATUS_PAGE: i32 = 113;
const ID_LOG: i32 = 114;
const ID_MESSAGE: i32 = 120;
const ID_APPS_NOTE: i32 = 121;
const ID_APPS_FIX: i32 = 122;
const ID_CONNECTIONS: i32 = 130;
const ID_LIST: i32 = 131;
const ID_GROUP: i32 = 140;
const ID_PORT_LABEL: i32 = 141;
const ID_PORT: i32 = 142;
const ID_AUTO: i32 = 143;
const ID_ADDR_LABEL: i32 = 144;
const ID_ADDR: i32 = 145;
const ID_DETECT: i32 = 146;
const ID_SYSPROXY: i32 = 147;
const ID_MIN_LABEL: i32 = 148;
const ID_MIN: i32 = 149;
const ID_MIN_UNIT: i32 = 150;
const ID_APPS_LABEL: i32 = 151;
const ID_APPS_STATE: i32 = 152;
const ID_APPS_ALLOW: i32 = 153;
const ID_RACE_LABEL: i32 = 154;
const ID_RACE: i32 = 155;
const ID_RACE_UNIT: i32 = 156;
const ID_VERBOSE: i32 = 157;
const ID_NOTE: i32 = 158;
const ID_LOGIN: i32 = 160;
const ID_FOOTER: i32 = 161;

const COLUMNS: [(&str, i32, bool); 9] = [
    // (title, width at 96 DPI, right-aligned)
    ("Connection", 130, false),
    ("Adapter", 100, false),
    ("Download", 90, true),
    ("Upload", 85, true),
    ("Open", 50, true),
    ("Total", 60, true),
    ("Connect", 65, true),
    ("Failures", 65, true),
    ("Downloaded", 85, true),
];

const fn rgb(r: u32, g: u32, b: u32) -> u32 {
    r | (g << 8) | (b << 16)
}
const GREEN: u32 = rgb(26, 127, 55);
const ORANGE: u32 = rgb(176, 110, 0);
const RED: u32 = rgb(207, 34, 46);

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

struct App {
    hwnd: HWND,
    dpi: u32,
    font: HFONT,
    title_font: HFONT,
    settings: Settings,
    port: Arc<AtomicU16>,
    latest: Arc<Mutex<Reading>>,
    previous: Option<Reading>,
    rates: Vec<(String, f64, f64)>,
    running: bool,
    child: Option<Child>,
    message: Option<(String, bool)>,
    windows_apps: Option<bool>,
    apps_job: Option<JoinHandle<Result<(), String>>>,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
    static START_SETTINGS: RefCell<Option<Settings>> = const { RefCell::new(None) };
}

/// Opens the window and runs it until it is closed.
pub fn run(settings: Settings) {
    START_SETTINGS.with(|s| *s.borrow_mut() = Some(settings));
    unsafe {
        let icc = INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_LISTVIEW_CLASSES | ICC_STANDARD_CLASSES,
        };
        InitCommonControlsEx(&icc);

        let instance = GetModuleHandleW(std::ptr::null());
        let class = wide("RustDispatchProxyGui");
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: 0,
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: instance,
            hIcon: LoadIconW(std::ptr::null_mut(), IDI_APPLICATION),
            hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
            hbrBackground: GetSysColorBrush(COLOR_WINDOW),
            lpszMenuName: std::ptr::null(),
            lpszClassName: class.as_ptr(),
            hIconSm: std::ptr::null_mut(),
        };
        RegisterClassExW(&wc);

        let dpi = GetDpiForSystem();
        let title = wide("rust-dispatch-proxy");
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            scale(800, dpi),
            scale(680, dpi),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            return;
        }
        ShowWindow(hwnd, SW_SHOW);

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            // Lets Tab move between controls and Enter/Space press buttons.
            if IsDialogMessageW(hwnd, &msg) == 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
}

fn scale(v: i32, dpi: u32) -> i32 {
    v * dpi as i32 / 96
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_CREATE => {
            let _ = lparam as *const CREATESTRUCTW;
            let settings = START_SETTINGS.with(|s| s.borrow_mut().take()).unwrap_or_default();
            let app = App::create(hwnd, settings);
            APP.with(|a| *a.borrow_mut() = Some(app));
            with_app(|app| {
                app.layout();
                app.refresh();
            });
            0
        }
        WM_DESTROY => {
            APP.with(|a| {
                if let Ok(mut a) = a.try_borrow_mut()
                    && let Some(app) = a.take()
                {
                    unsafe {
                        DeleteObject(app.font);
                        DeleteObject(app.title_font);
                    }
                }
            });
            unsafe { PostQuitMessage(0) };
            0
        }
        WM_SIZE => {
            with_app(|app| app.layout());
            0
        }
        WM_GETMINMAXINFO => {
            let info = lparam as *mut MINMAXINFO;
            let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
            unsafe {
                (*info).ptMinTrackSize.x = scale(720, dpi);
                (*info).ptMinTrackSize.y = scale(660, dpi);
            }
            0
        }
        WM_DPICHANGED => {
            let suggested = lparam as *const RECT;
            unsafe {
                let r = *suggested;
                SetWindowPos(
                    hwnd,
                    std::ptr::null_mut(),
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            with_app(|app| {
                app.dpi = (wparam >> 16) as u32 & 0xFFFF;
                app.make_fonts();
                app.layout();
            });
            0
        }
        WM_STATUS => {
            with_app(|app| app.refresh());
            0
        }
        WM_APPS_DONE => {
            with_app(|app| app.finish_apps_job());
            0
        }
        WM_COMMAND => {
            let id = (wparam & 0xFFFF) as i32;
            let code = ((wparam >> 16) & 0xFFFF) as u32;
            with_app(|app| app.command(id, code));
            0
        }
        WM_CTLCOLORSTATIC => {
            let hdc = wparam as HDC;
            let ctl = lparam as HWND;
            let color = APP.with(|a| a.try_borrow().ok().and_then(|a| a.as_ref().and_then(|app| app.color_of(ctl))));
            unsafe {
                SetBkMode(hdc, TRANSPARENT as i32);
                SetTextColor(
                    hdc,
                    color.unwrap_or_else(|| GetSysColor(windows_sys::Win32::Graphics::Gdi::COLOR_WINDOWTEXT)),
                );
                GetSysColorBrush(COLOR_WINDOW) as LRESULT
            }
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Runs `f` on the app unless it's already in use further up the call
/// stack (setting a control's text can send messages back to the window).
fn with_app(f: impl FnOnce(&mut App)) {
    APP.with(|a| {
        if let Ok(mut a) = a.try_borrow_mut()
            && let Some(app) = a.as_mut()
        {
            f(app);
        }
    });
}

impl App {
    fn create(hwnd: HWND, settings: Settings) -> App {
        let port = Arc::new(AtomicU16::new(settings.port));
        let target = hwnd as usize;
        let latest = status::spawn_poller(port.clone(), move || unsafe {
            PostMessageW(target as HWND, WM_STATUS, 0, 0);
        });
        let mut app = App {
            hwnd,
            dpi: unsafe { GetDpiForWindow(hwnd) }.max(96),
            font: std::ptr::null_mut(),
            title_font: std::ptr::null_mut(),
            settings,
            port,
            latest,
            previous: None,
            rates: Vec::new(),
            running: false,
            child: None,
            message: None,
            windows_apps: control::windows_apps_allowed(),
            apps_job: None,
        };
        app.make_controls();
        app.make_fonts();
        app
    }

    fn ctl(&self, id: i32) -> HWND {
        unsafe { GetDlgItem(self.hwnd, id) }
    }

    fn child(&self, class: &str, text: &str, id: i32, style: u32, ex_style: u32) {
        let (class, text) = (wide(class), wide(text));
        unsafe {
            CreateWindowExW(
                ex_style,
                class.as_ptr(),
                text.as_ptr(),
                WS_CHILD | WS_VISIBLE | style,
                0,
                0,
                10,
                10,
                self.hwnd,
                id as usize as HMENU,
                GetModuleHandleW(std::ptr::null()),
                std::ptr::null(),
            );
        }
    }

    fn make_controls(&mut self) {
        let label = |app: &App, text: &str, id: i32| app.child("STATIC", text, id, 0, 0);
        let button = |app: &App, text: &str, id: i32| app.child("BUTTON", text, id, WS_TABSTOP | BS_PUSHBUTTON as u32, 0);
        let checkbox = |app: &App, text: &str, id: i32| app.child("BUTTON", text, id, WS_TABSTOP | BS_AUTOCHECKBOX as u32, 0);
        let edit = |app: &App, text: &str, id: i32, numbers: bool| {
            let style = WS_TABSTOP | ES_AUTOHSCROLL as u32 | if numbers { ES_NUMBER as u32 } else { 0 };
            app.child("EDIT", text, id, style, WS_EX_CLIENTEDGE);
        };

        label(self, "rust-dispatch-proxy", ID_TITLE);
        label(self, "", ID_STATE);
        label(self, "", ID_DETAILS);
        button(self, "Start", ID_START);
        button(self, "Stop", ID_STOP);
        button(self, "Restart", ID_RESTART);
        button(self, "Open status page", ID_STATUS_PAGE);
        button(self, "Open log", ID_LOG);
        label(self, "", ID_MESSAGE);
        label(
            self,
            "Microsoft Store and other Windows apps can't connect while the proxy is on.",
            ID_APPS_NOTE,
        );
        button(self, "Fix (needs administrator)", ID_APPS_FIX);
        label(self, "Connections", ID_CONNECTIONS);

        // The connections table.
        let class = WC_LISTVIEWW;
        let empty = wide("");
        unsafe {
            CreateWindowExW(
                WS_EX_CLIENTEDGE,
                class,
                empty.as_ptr(),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | LVS_REPORT | LVS_SINGLESEL | LVS_NOSORTHEADER,
                0,
                0,
                10,
                10,
                self.hwnd,
                ID_LIST as usize as HMENU,
                GetModuleHandleW(std::ptr::null()),
                std::ptr::null(),
            );
            let list = self.ctl(ID_LIST);
            let ex = (LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER) as isize;
            SendMessageW(list, LVM_SETEXTENDEDLISTVIEWSTYLE, ex as usize, ex);
            for (i, (title, _, right)) in COLUMNS.iter().enumerate() {
                let mut text = wide(title);
                let col = LVCOLUMNW {
                    mask: LVCF_FMT | LVCF_WIDTH | LVCF_TEXT,
                    fmt: if *right && i > 0 { LVCFMT_RIGHT } else { LVCFMT_LEFT },
                    cx: 80,
                    pszText: text.as_mut_ptr(),
                    ..std::mem::zeroed()
                };
                SendMessageW(list, LVM_INSERTCOLUMNW, i, &col as *const _ as LPARAM);
            }
        }

        self.child("BUTTON", "Settings", ID_GROUP, BS_GROUPBOX as u32, 0);
        label(self, "Port", ID_PORT_LABEL);
        edit(self, &self.settings.port.to_string(), ID_PORT, true);
        checkbox(self, "Find my internet connections automatically", ID_AUTO);
        label(self, "Addresses", ID_ADDR_LABEL);
        edit(self, &self.settings.addresses, ID_ADDR, false);
        button(self, "Detect", ID_DETECT);
        checkbox(self, "Turn on the Windows proxy setting while the proxy runs", ID_SYSPROXY);
        label(self, "only with at least", ID_MIN_LABEL);
        edit(self, &self.settings.min_links.to_string(), ID_MIN, true);
        label(self, "connections", ID_MIN_UNIT);
        label(self, "Microsoft Store and other Windows apps:", ID_APPS_LABEL);
        label(self, "", ID_APPS_STATE);
        button(self, "Allow", ID_APPS_ALLOW);
        label(self, "Try the next connection after", ID_RACE_LABEL);
        edit(self, &self.settings.race_ms.to_string(), ID_RACE, true);
        label(self, "ms  (0 = only after a failure)", ID_RACE_UNIT);
        checkbox(self, "Log every connection", ID_VERBOSE);
        label(
            self,
            "Settings are saved automatically and take effect when the proxy (re)starts.",
            ID_NOTE,
        );
        checkbox(self, "Start the proxy automatically when I log in", ID_LOGIN);
        label(self, "Closing this window doesn't stop the proxy.", ID_FOOTER);

        self.set_check(ID_AUTO, self.settings.auto);
        self.set_check(ID_SYSPROXY, self.settings.system_proxy);
        self.set_check(ID_VERBOSE, self.settings.verbose);
        self.set_check(ID_LOGIN, control::starts_at_login());
        // Shown only when there is something to say; layout() leaves them out.
        for id in [ID_MESSAGE, ID_APPS_NOTE, ID_APPS_FIX] {
            self.show(id, false);
        }
    }

    fn make_fonts(&mut self) {
        let face = wide("Segoe UI");
        let make = |points: i32, weight: i32| unsafe {
            CreateFontW(
                -(points * self.dpi as i32 / 72),
                0,
                0,
                0,
                weight,
                0,
                0,
                0,
                DEFAULT_CHARSET as u32,
                OUT_DEFAULT_PRECIS as u32,
                CLIP_DEFAULT_PRECIS as u32,
                CLEARTYPE_QUALITY as u32,
                0,
                face.as_ptr(),
            )
        };
        let (font, title_font) = (make(9, FW_NORMAL as i32), make(14, FW_SEMIBOLD as i32));
        let (old, old_title) = (self.font, self.title_font);
        self.font = font;
        self.title_font = title_font;
        for id in ID_TITLE..=ID_FOOTER {
            let ctl = self.ctl(id);
            if !ctl.is_null() {
                let f = if id == ID_TITLE { title_font } else { font };
                unsafe { SendMessageW(ctl, WM_SETFONT, f as usize, 1) };
            }
        }
        unsafe {
            if !old.is_null() {
                DeleteObject(old);
            }
            if !old_title.is_null() {
                DeleteObject(old_title);
            }
        }
    }

    /// Places every control; sizes scale with the window's DPI.
    fn layout(&self) {
        let d = self.dpi;
        let s = |v: i32| scale(v, d);
        let mut client = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        unsafe { GetClientRect(self.hwnd, &mut client) };
        let width = client.right;
        let place = |id: i32, x: i32, y: i32, w: i32, h: i32| unsafe {
            MoveWindow(self.ctl(id), s(x), s(y), s(w), s(h), 1);
        };
        let full = |id: i32, x: i32, y: i32, h: i32| unsafe {
            MoveWindow(self.ctl(id), s(x), s(y), width - s(x) - s(14), s(h), 1);
        };

        place(ID_TITLE, 14, 10, 240, 30);
        place(ID_STATE, 262, 18, 80, 20);
        full(ID_DETAILS, 342, 18, 20);
        place(ID_START, 14, 48, 80, 28);
        place(ID_STOP, 100, 48, 80, 28);
        place(ID_RESTART, 186, 48, 90, 28);
        place(ID_STATUS_PAGE, 292, 48, 130, 28);
        place(ID_LOG, 428, 48, 90, 28);

        // The message line and the Windows-apps notice take room only when shown.
        let mut y = 86;
        if self.message.is_some() {
            full(ID_MESSAGE, 14, y, 20);
            y += 24;
        }
        if self.apps_blocked() {
            place(ID_APPS_NOTE, 14, y + 6, 470, 20);
            place(ID_APPS_FIX, 490, y, 190, 28);
            y += 34;
        }
        full(ID_CONNECTIONS, 14, y + 4, 20);
        full(ID_LIST, 14, y + 24, 150);

        let gy = y + 186;
        full(ID_GROUP, 14, gy, 286);
        place(ID_PORT_LABEL, 28, gy + 28, 60, 20);
        place(ID_PORT, 92, gy + 25, 70, 24);
        place(ID_AUTO, 28, gy + 56, 420, 22);
        place(ID_ADDR_LABEL, 48, gy + 86, 70, 20);
        place(ID_ADDR, 122, gy + 83, 330, 24);
        place(ID_DETECT, 458, gy + 82, 80, 26);
        place(ID_SYSPROXY, 28, gy + 114, 460, 22);
        place(ID_MIN_LABEL, 48, gy + 142, 120, 20);
        place(ID_MIN, 170, gy + 139, 46, 24);
        place(ID_MIN_UNIT, 222, gy + 142, 120, 20);
        place(ID_APPS_LABEL, 48, gy + 172, 260, 20);
        place(ID_APPS_STATE, 310, gy + 172, 70, 20);
        place(ID_APPS_ALLOW, 384, gy + 166, 260, 28);
        place(ID_RACE_LABEL, 28, gy + 204, 190, 20);
        place(ID_RACE, 220, gy + 201, 62, 24);
        place(ID_RACE_UNIT, 288, gy + 204, 260, 20);
        place(ID_VERBOSE, 28, gy + 230, 300, 22);
        place(ID_NOTE, 28, gy + 258, 600, 20);
        place(ID_LOGIN, 14, gy + 298, 460, 22);
        full(ID_FOOTER, 14, gy + 328, 20);

        // Stretch the first column so the columns exactly fill the table
        // (its inside width, without border or scroll bar).
        let list = self.ctl(ID_LIST);
        let mut inside = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        unsafe { GetClientRect(list, &mut inside) };
        let fixed: i32 = COLUMNS.iter().skip(1).map(|c| s(c.1)).sum();
        let first = (inside.right - fixed - 1).max(s(110));
        unsafe {
            SendMessageW(list, LVM_SETCOLUMNWIDTH, 0, first as LPARAM);
            for (i, c) in COLUMNS.iter().enumerate().skip(1) {
                SendMessageW(list, LVM_SETCOLUMNWIDTH, i, s(c.1) as LPARAM);
            }
        }
    }

    fn set_check(&self, id: i32, on: bool) {
        let state = if on { BST_CHECKED } else { BST_UNCHECKED };
        unsafe { SendMessageW(self.ctl(id), BM_SETCHECK, state as usize, 0) };
    }

    fn is_checked(&self, id: i32) -> bool {
        unsafe { SendMessageW(self.ctl(id), BM_GETCHECK, 0, 0) == BST_CHECKED as isize }
    }

    fn text(&self, id: i32) -> String {
        let ctl = self.ctl(id);
        unsafe {
            let len = GetWindowTextLengthW(ctl).max(0) as usize;
            let mut buf = vec![0u16; len + 1];
            let n = GetWindowTextW(ctl, buf.as_mut_ptr(), buf.len() as i32).max(0) as usize;
            String::from_utf16_lossy(&buf[..n])
        }
    }

    fn set_text(&self, id: i32, text: &str) {
        if self.text(id) != text {
            let w = wide(text);
            unsafe { SetWindowTextW(self.ctl(id), w.as_ptr()) };
        }
    }

    fn enable(&self, id: i32, on: bool) {
        unsafe { EnableWindow(self.ctl(id), on as i32) };
    }

    fn show(&self, id: i32, on: bool) {
        unsafe { ShowWindow(self.ctl(id), if on { SW_SHOW } else { SW_HIDE }) };
    }

    /// Text colour for a static control, if it isn't the default.
    fn color_of(&self, ctl: HWND) -> Option<u32> {
        let is = |id: i32| ctl == self.ctl(id);
        if is(ID_STATE) {
            Some(if self.running {
                GREEN
            } else {
                unsafe { GetSysColor(windows_sys::Win32::Graphics::Gdi::COLOR_GRAYTEXT) }
            })
        } else if is(ID_MESSAGE) {
            match &self.message {
                Some((_, true)) => Some(RED),
                _ => Some(unsafe { GetSysColor(windows_sys::Win32::Graphics::Gdi::COLOR_GRAYTEXT) }),
            }
        } else if is(ID_APPS_NOTE) {
            Some(ORANGE)
        } else if is(ID_APPS_STATE) {
            Some(if self.windows_apps == Some(true) { GREEN } else { ORANGE })
        } else if is(ID_DETAILS) || is(ID_NOTE) || is(ID_FOOTER) {
            Some(unsafe { GetSysColor(windows_sys::Win32::Graphics::Gdi::COLOR_GRAYTEXT) })
        } else {
            None
        }
    }

    fn info(&mut self, text: impl Into<String>) {
        self.message = Some((text.into(), false));
        self.show_message();
    }

    fn error(&mut self, text: impl Into<String>) {
        self.message = Some((text.into(), true));
        self.show_message();
    }

    fn show_message(&self) {
        let text = self.message.as_ref().map(|m| m.0.as_str()).unwrap_or("");
        self.set_text(ID_MESSAGE, text);
        unsafe { InvalidateRect(self.ctl(ID_MESSAGE), std::ptr::null(), 1) };
        self.set_visible(ID_MESSAGE, self.message.is_some());
    }

    fn apps_blocked(&self) -> bool {
        self.settings.system_proxy && self.windows_apps == Some(false)
    }

    /// Shows or hides a control that takes room in the layout, and re-lays
    /// out the window if that changed.
    fn set_visible(&self, id: i32, on: bool) {
        let visible = unsafe { IsWindowVisible(self.ctl(id)) } != 0;
        if visible != on {
            self.show(id, on);
            self.layout();
            unsafe { InvalidateRect(self.hwnd, std::ptr::null(), 1) };
        }
    }

    // -------------------------------------------------------------- actions

    fn command(&mut self, id: i32, code: u32) {
        match (id, code) {
            (ID_START, BN_CLICKED) => self.start(),
            (ID_STOP, BN_CLICKED) => self.stop(),
            (ID_RESTART, BN_CLICKED) => {
                self.stop();
                self.start();
            }
            (ID_STATUS_PAGE, BN_CLICKED) => control::open(&format!("http://127.0.0.1:{}/status", self.settings.port)),
            (ID_LOG, BN_CLICKED) => {
                let log = settings::log_path();
                if log.exists() {
                    control::open(&log.to_string_lossy());
                } else {
                    self.info("No log yet: it is created when the proxy starts from here.");
                }
            }
            (ID_APPS_FIX | ID_APPS_ALLOW, BN_CLICKED) => self.allow_windows_apps(),
            (ID_DETECT, BN_CLICKED) => match control::detect_addresses() {
                Ok(found) => self.set_text(ID_ADDR, &found), // EN_CHANGE saves it
                Err(e) => self.error(e),
            },
            (ID_LOGIN, BN_CLICKED) => {
                let on = self.is_checked(ID_LOGIN);
                if let Err(e) = control::set_start_at_login(on) {
                    self.set_check(ID_LOGIN, !on);
                    self.error(e);
                }
            }
            (ID_AUTO | ID_SYSPROXY | ID_VERBOSE, BN_CLICKED) | (ID_PORT | ID_ADDR | ID_MIN | ID_RACE, EN_CHANGE) => self.read_settings(),
            _ => {}
        }
    }

    /// Takes the settings from the controls and saves them.
    fn read_settings(&mut self) {
        let mut s = self.settings.clone();
        s.auto = self.is_checked(ID_AUTO);
        s.system_proxy = self.is_checked(ID_SYSPROXY);
        s.verbose = self.is_checked(ID_VERBOSE);
        s.addresses = self.text(ID_ADDR);
        if let Ok(port) = self.text(ID_PORT).parse::<u16>()
            && port > 0
        {
            s.port = port;
        }
        if let Ok(n) = self.text(ID_MIN).parse::<u32>() {
            s.min_links = n.clamp(1, 16);
        }
        if let Ok(ms) = self.text(ID_RACE).parse::<u64>() {
            s.race_ms = ms.min(5000);
        }
        if s != self.settings {
            self.settings = s;
            self.port.store(self.settings.port, Relaxed);
            if let Err(e) = self.settings.save() {
                self.error(format!("Could not save settings: {e}"));
            }
        }
        self.update_enabled();
    }

    fn start(&mut self) {
        match control::start(&self.settings) {
            Ok(child) => {
                self.child = Some(child);
                self.info("Starting...");
            }
            Err(e) => self.error(e),
        }
    }

    fn stop(&mut self) {
        self.child = None;
        match control::stop(self.settings.port) {
            Ok(()) => self.info("Stopped. The Windows proxy setting is off."),
            Err(e) => self.error(e),
        }
    }

    fn allow_windows_apps(&mut self) {
        if self.apps_job.is_some() {
            return;
        }
        self.info("Waiting for administrator permission...");
        let target = self.hwnd as usize;
        self.apps_job = Some(std::thread::spawn(move || {
            let result = control::allow_windows_apps();
            unsafe { PostMessageW(target as HWND, WM_APPS_DONE, 0, 0) };
            result
        }));
        self.update_enabled();
    }

    fn finish_apps_job(&mut self) {
        let Some(job) = self.apps_job.take() else { return };
        let result = job.join().unwrap_or_else(|_| Err("Failed".into()));
        self.windows_apps = control::windows_apps_allowed();
        match result {
            Ok(()) => self.info("Done: Microsoft Store and other Windows apps can now use the proxy."),
            Err(e) => self.error(e),
        }
        self.refresh();
    }

    /// Notices if the proxy this window started exited (bad settings, port
    /// already in use, ...), and shows why.
    fn check_child(&mut self) {
        let exited = match self.child.as_mut().map(|c| c.try_wait()) {
            Some(Ok(Some(code))) => Some(code),
            _ => None,
        };
        if let Some(code) = exited {
            self.child = None;
            let line = std::fs::read_to_string(settings::log_path()).ok().and_then(|log| {
                log.lines()
                    .rev()
                    .find(|l| l.contains("[FATAL]") || l.contains("[ERROR]"))
                    .map(String::from)
            });
            self.error(match line {
                Some(line) => format!("The proxy stopped: {line}"),
                None => format!("The proxy stopped ({code}). See the log for details."),
            });
        }
    }

    // -------------------------------------------------------------- display

    fn update_enabled(&self) {
        self.enable(ID_START, !self.running);
        self.enable(ID_STOP, self.running);
        self.enable(ID_RESTART, self.running);
        self.enable(ID_STATUS_PAGE, self.running);
        let manual = !self.settings.auto;
        self.enable(ID_ADDR_LABEL, manual);
        self.enable(ID_ADDR, manual);
        self.enable(ID_DETECT, manual);
        let sys = self.settings.system_proxy;
        for id in [ID_MIN_LABEL, ID_MIN, ID_MIN_UNIT, ID_APPS_LABEL, ID_APPS_STATE] {
            self.enable(id, sys);
        }
        let idle = self.apps_job.is_none();
        self.enable(ID_APPS_ALLOW, sys && idle);
        self.enable(ID_APPS_FIX, idle);
        let blocked = self.apps_blocked();
        self.show(ID_APPS_FIX, blocked);
        self.set_visible(ID_APPS_NOTE, blocked);
    }

    /// Updates everything from the latest status reading.
    fn refresh(&mut self) {
        self.check_child();
        let reading = self.latest.lock().unwrap_or_else(|e| e.into_inner()).clone();
        self.update_rates(&reading);

        let was_running = self.running;
        self.running = reading.status.is_some();
        match &reading.status {
            Some(s) => {
                let (h, m) = (s.uptime_s / 3600, s.uptime_s % 3600 / 60);
                self.set_text(ID_STATE, "● Running");
                self.set_text(ID_DETAILS, &format!("v{} · up {h}h {m}m · port {}", s.version, self.settings.port));
            }
            None => {
                self.set_text(ID_STATE, "● Stopped");
                self.set_text(ID_DETAILS, "");
            }
        }
        if was_running != self.running {
            unsafe { InvalidateRect(self.ctl(ID_STATE), std::ptr::null(), 1) };
        }
        self.set_text(
            ID_APPS_STATE,
            match self.windows_apps {
                Some(true) => "allowed",
                Some(false) => "blocked",
                None => "unknown",
            },
        );
        self.set_text(
            ID_APPS_ALLOW,
            if self.windows_apps == Some(true) {
                "Allow again (for newly installed apps)"
            } else {
                "Allow"
            },
        );
        self.update_enabled();
        self.fill_table(&reading);
    }

    fn update_rates(&mut self, now: &Reading) {
        if self.previous.as_ref().is_some_and(|p| p.at == now.at) {
            return;
        }
        if let (Some(prev), Some(cur)) = (self.previous.as_ref().and_then(|p| p.status.as_ref().map(|s| (p.at, s))), &now.status) {
            let secs = now.at.duration_since(prev.0).as_secs_f64().max(0.001);
            self.rates = cur
                .links
                .iter()
                .map(|l| {
                    let old = prev.1.links.iter().find(|o| o.link == l.link);
                    let rate = |new: u64, old: Option<u64>| old.map_or(0.0, |o| new.saturating_sub(o) as f64 * 8.0 / 1e6 / secs);
                    (
                        l.link.clone(),
                        rate(l.bytes_down, old.map(|o| o.bytes_down)),
                        rate(l.bytes_up, old.map(|o| o.bytes_up)),
                    )
                })
                .collect();
        }
        self.previous = Some(now.clone());
    }

    fn fill_table(&self, reading: &Reading) {
        let mut rows: Vec<[String; 9]> = Vec::new();
        // When there's nothing to list, the heading says why.
        self.set_text(
            ID_CONNECTIONS,
            match &reading.status {
                None => "Connections: the proxy isn't running. Press Start.",
                Some(s) if s.links.is_empty() => "Connections: no internet connections found yet.",
                Some(_) => "Connections",
            },
        );
        match &reading.status {
            None => {}
            Some(s) if s.links.is_empty() => {}
            Some(s) => {
                let (mut down_sum, mut up_sum, mut open_sum, mut total_down) = (0.0, 0.0, 0, 0);
                for l in &s.links {
                    let (down, up) = self.rates.iter().find(|r| r.0 == l.link).map_or((0.0, 0.0), |r| (r.1, r.2));
                    down_sum += down;
                    up_sum += up;
                    open_sum += l.active;
                    total_down += l.bytes_down;
                    let adapter = if l.cooldown_s > 0 {
                        format!("{} (cooling down {}s)", l.adapter, l.cooldown_s)
                    } else {
                        l.adapter.clone()
                    };
                    rows.push([
                        l.link.clone(),
                        adapter,
                        format!("{down:.1} Mbit/s"),
                        format!("{up:.1} Mbit/s"),
                        l.active.to_string(),
                        l.connections.to_string(),
                        if l.connect_ms > 0.0 {
                            format!("{:.0} ms", l.connect_ms)
                        } else {
                            "-".into()
                        },
                        l.failures.to_string(),
                        size(l.bytes_down),
                    ]);
                }
                rows.push([
                    "Total".into(),
                    "".into(),
                    format!("{down_sum:.1} Mbit/s"),
                    format!("{up_sum:.1} Mbit/s"),
                    open_sum.to_string(),
                    "".into(),
                    "".into(),
                    "".into(),
                    size(total_down),
                ]);
            }
        }

        let list = self.ctl(ID_LIST);
        unsafe {
            let count = SendMessageW(list, LVM_GETITEMCOUNT, 0, 0) as usize;
            if count != rows.len() {
                SendMessageW(list, LVM_DELETEALLITEMS, 0, 0);
                for i in 0..rows.len() {
                    let mut empty = wide("");
                    let item = LVITEMW {
                        mask: LVIF_TEXT,
                        iItem: i as i32,
                        pszText: empty.as_mut_ptr(),
                        ..std::mem::zeroed()
                    };
                    SendMessageW(list, LVM_INSERTITEMW, 0, &item as *const _ as LPARAM);
                }
            }
            // Only cells whose text changed are updated, which avoids flicker.
            for (i, row) in rows.iter().enumerate() {
                for (col, text) in row.iter().enumerate() {
                    let mut buf = [0u16; 128];
                    let mut probe = LVITEMW {
                        iSubItem: col as i32,
                        pszText: buf.as_mut_ptr(),
                        cchTextMax: 128,
                        ..std::mem::zeroed()
                    };
                    let n = SendMessageW(
                        list,
                        windows_sys::Win32::UI::Controls::LVM_GETITEMTEXTW,
                        i,
                        &mut probe as *mut _ as LPARAM,
                    ) as usize;
                    if String::from_utf16_lossy(&buf[..n.min(128)]) != *text {
                        let mut w = wide(text);
                        let item = LVITEMW {
                            iSubItem: col as i32,
                            pszText: w.as_mut_ptr(),
                            ..std::mem::zeroed()
                        };
                        SendMessageW(list, LVM_SETITEMTEXTW, i, &item as *const _ as LPARAM);
                    }
                }
            }
        }
    }
}

fn size(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.2} GB", bytes as f64 / 1e9)
    } else {
        format!("{:.1} MB", bytes as f64 / 1e6)
    }
}
