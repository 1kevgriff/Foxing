//! UI Automation provider for Foxing's custom components. Each host window exposes an
//! accessibility tree ([`Node`]) through one generic COM object per element, built
//! from raw vtables (no COM crate). UIAutomationCore and oleaut32 are loaded on the
//! first `WM_GETOBJECT`, so there is no startup cost when no screen reader is running.

use foxing::buffer::Buffer;
use foxing::textunits::{self, Unit};
use foxing::ui::a11y::{Action, Node, Role};
use foxing::ui::Rect;
use std::cell::Cell;
use std::ffi::c_void;
use std::mem::offset_of;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::OnceLock;
use windows_sys::core::{GUID, HRESULT};
use windows_sys::s;
use windows_sys::w;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::{ClientToScreen, ScreenToClient};
use windows_sys::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

/// How a host window describes and drives its accessibility tree.
pub struct Source {
    pub tree: unsafe fn(HWND) -> Node,
    pub act: unsafe fn(HWND, Action),
    pub class: &'static str,
    /// Document text access (UIA Text pattern on the root element), if any.
    pub text: Option<&'static TextSource>,
}

/// Reads the document: runs the closure with the buffer; false if unavailable.
pub type ReadFn = unsafe fn(HWND, &mut dyn FnMut(&Buffer)) -> bool;

/// Host callbacks behind the Text pattern. Offsets are buffer byte offsets.
pub struct TextSource {
    /// Runs `f` with the buffer; false if it isn't available right now.
    pub read: ReadFn,
    pub selection: unsafe fn(HWND) -> (usize, usize),
    pub select: unsafe fn(HWND, usize, usize),
    /// First and end offsets of what's on screen.
    pub visible: unsafe fn(HWND) -> (usize, usize),
    /// Client-coordinate rectangles covering `start..end` where it's visible.
    pub rects: unsafe fn(HWND, usize, usize) -> Vec<Rect>,
    pub offset_at: unsafe fn(HWND, i32, i32) -> usize,
    pub scroll_to: unsafe fn(HWND, usize),
}

// ---- constants ----

const UIA_ROOT_OBJECT_ID: i32 = -25;
const E_NOINTERFACE: HRESULT = 0x8000_4002_u32 as i32;
const E_INVALIDARG: HRESULT = 0x8007_0057_u32 as i32;
const E_NOTIMPL: HRESULT = 0x8000_4001_u32 as i32;
const UIA_E_ELEMENTNOTAVAILABLE: HRESULT = 0x8004_0201_u32 as i32;
const PROVIDER_OPTIONS: i32 = 0x2 | 0x20; // ServerSideProvider | UseComThreading
const APPEND_RUNTIME_ID: i32 = 3;
const FOCUS_CHANGED_EVENT: i32 = 20005;
pub const TEXT_SELECTION_CHANGED_EVENT: i32 = 20014;
pub const TEXT_CHANGED_EVENT: i32 = 20015;

/// Clients subscribed to text events on our documents (via AdviseEventAdded). Text
/// events fire on every keystroke, so they're raised only while someone listens.
static TEXT_SUBSCRIBERS: AtomicI32 = AtomicI32::new(0);

/// Longest text exposed through the Value pattern; the Text pattern serves any size.
const VALUE_CHARS: usize = 100_000;

const VT_EMPTY: u16 = 0;
const VT_I4: u16 = 3;
const VT_BSTR: u16 = 8;
const VT_R8: u16 = 5;
const VT_BOOL: u16 = 11;
const VT_UNKNOWN: u16 = 13;

const IID_IUNKNOWN: GUID = GUID::from_u128(0x00000000_0000_0000_c000_000000000046);
const IID_SIMPLE: GUID = GUID::from_u128(0xd6dd68d1_86fd_4332_8666_9abedea2d24c);
const IID_FRAGMENT: GUID = GUID::from_u128(0xf7063da8_8359_439c_9297_bbc5299a7d87);
const IID_ROOT: GUID = GUID::from_u128(0x620ce2a5_ab8f_40a9_86cb_de3c75599b58);
const IID_INVOKE: GUID = GUID::from_u128(0x54fcb24b_e18e_47a2_b4d3_eccbe77599a2);
const IID_TOGGLE: GUID = GUID::from_u128(0x56d00bd0_c4f4_433c_a836_1a52a57e0892);
const IID_EXPAND: GUID = GUID::from_u128(0xd847d3a5_cab0_4a98_8c32_ecb45c59ad24);
const IID_SELITEM: GUID = GUID::from_u128(0x2acad808_b2d4_452d_a407_91ff1ad167b2);
const IID_VALUE: GUID = GUID::from_u128(0xc7935180_6fb3_4201_b174_7df73adbf64a);
const IID_TEXT: GUID = GUID::from_u128(0x3589c92c_63f3_4367_99bb_ada653b77cf2);
const IID_RANGE: GUID = GUID::from_u128(0x5347ad7b_c355_46f8_aff5_909033582f63);
const IID_ADVISE: GUID = GUID::from_u128(0xa407b27b_0f6d_4427_9292_473c7bf93258);

const PATTERN_INVOKE: i32 = 10000;
const PATTERN_VALUE: i32 = 10002;
const PATTERN_EXPAND: i32 = 10005;
const PATTERN_SELITEM: i32 = 10010;
const PATTERN_TEXT: i32 = 10014;
const PATTERN_TOGGLE: i32 = 10015;

fn control_type(role: Role) -> i32 {
    match role {
        Role::Document => 50030,
        Role::MenuBar => 50010,
        Role::Menu => 50009,
        Role::MenuItem => 50011,
        Role::Separator => 50038,
        Role::List => 50008,
        Role::ListItem => 50007,
        Role::StatusBar => 50017,
        Role::Text => 50020,
    }
}

fn same(a: &GUID, b: &GUID) -> bool {
    a.data1 == b.data1 && a.data2 == b.data2 && a.data3 == b.data3 && a.data4 == b.data4
}

// ---- lazily loaded API ----

type FnReturnProvider = unsafe extern "system" fn(HWND, WPARAM, LPARAM, *mut c_void) -> LRESULT;
type FnHostFromHwnd = unsafe extern "system" fn(HWND, *mut *mut c_void) -> HRESULT;
type FnRaiseEvent = unsafe extern "system" fn(*mut c_void, i32) -> HRESULT;
type FnClientsListening = unsafe extern "system" fn() -> i32;
type FnSysAllocString = unsafe extern "system" fn(*const u16) -> *mut u16;
type FnSaCreateVector = unsafe extern "system" fn(u16, i32, u32) -> *mut c_void;
type FnSaPut = unsafe extern "system" fn(*mut c_void, *const i32, *const c_void) -> HRESULT;
type FnNotSupported = unsafe extern "system" fn(*mut *mut c_void) -> HRESULT;

struct Api {
    return_provider: FnReturnProvider,
    host_from_hwnd: FnHostFromHwnd,
    raise_event: FnRaiseEvent,
    clients_listening: FnClientsListening,
    sys_alloc_string: FnSysAllocString,
    sa_create_vector: FnSaCreateVector,
    sa_put: FnSaPut,
    not_supported: FnNotSupported,
}

static API: OnceLock<Option<Api>> = OnceLock::new();

fn api() -> Option<&'static Api> {
    API.get_or_init(|| unsafe {
        let uia = LoadLibraryW(w!("UIAutomationCore.dll"));
        let ole = LoadLibraryW(w!("oleaut32.dll"));
        if uia.is_null() || ole.is_null() {
            return None;
        }
        macro_rules! get {
            ($lib:expr, $name:expr, $ty:ty) => {
                std::mem::transmute::<unsafe extern "system" fn() -> isize, $ty>(GetProcAddress(
                    $lib, $name,
                )?)
            };
        }
        CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32);
        Some(Api {
            return_provider: get!(uia, s!("UiaReturnRawElementProvider"), FnReturnProvider),
            host_from_hwnd: get!(uia, s!("UiaHostProviderFromHwnd"), FnHostFromHwnd),
            raise_event: get!(uia, s!("UiaRaiseAutomationEvent"), FnRaiseEvent),
            clients_listening: get!(uia, s!("UiaClientsAreListening"), FnClientsListening),
            sys_alloc_string: get!(ole, s!("SysAllocString"), FnSysAllocString),
            sa_create_vector: get!(ole, s!("SafeArrayCreateVector"), FnSaCreateVector),
            sa_put: get!(ole, s!("SafeArrayPutElement"), FnSaPut),
            not_supported: get!(uia, s!("UiaGetReservedNotSupportedValue"), FnNotSupported),
        })
    })
    .as_ref()
}

/// Answers `WM_GETOBJECT` for `hwnd`'s tree; `None` means "not for UIA, use the default".
pub unsafe fn get_object(
    hwnd: HWND,
    wp: WPARAM,
    lp: LPARAM,
    src: &'static Source,
) -> Option<LRESULT> {
    if lp as i32 != UIA_ROOT_OBJECT_ID {
        return None;
    }
    let api = api()?;
    let p = Provider::create(hwnd, src, Vec::new());
    let r = (api.return_provider)(
        hwnd,
        wp,
        lp,
        Provider::iface(p, offset_of!(Provider, simple)),
    );
    Provider::release_raw(p);
    Some(r)
}

/// Raises `event` on `hwnd`'s root element (e.g. text or selection changed). Free when
/// nobody is listening.
pub unsafe fn raise(hwnd: HWND, src: &'static Source, event: i32) {
    let text_event = event == TEXT_SELECTION_CHANGED_EVENT || event == TEXT_CHANGED_EVENT;
    if text_event && TEXT_SUBSCRIBERS.load(Ordering::Relaxed) <= 0 {
        return;
    }
    let Some(api) = API.get().and_then(|a| a.as_ref()) else {
        return;
    };
    if (api.clients_listening)() == 0 {
        return;
    }
    let p = Provider::create(hwnd, src, Vec::new());
    (api.raise_event)(Provider::iface(p, offset_of!(Provider, simple)), event);
    Provider::release_raw(p);
}

/// Tells listening screen readers that focus moved within `hwnd`'s tree (e.g. to a
/// menu item). Free when nobody is listening.
pub unsafe fn focus_changed(hwnd: HWND, src: &'static Source) {
    let Some(api) = API.get().and_then(|a| a.as_ref()) else {
        return;
    };
    if (api.clients_listening)() == 0 {
        return;
    }
    let Some(path) = (src.tree)(hwnd).focus_path() else {
        return;
    };
    let p = Provider::create(hwnd, src, path);
    (api.raise_event)(
        Provider::iface(p, offset_of!(Provider, simple)),
        FOCUS_CHANGED_EVENT,
    );
    Provider::release_raw(p);
}

// ---- VARIANT (24 bytes on x64) ----

#[repr(C)]
struct Variant {
    vt: u16,
    reserved: [u16; 3],
    val: [u64; 2],
}

unsafe fn set_i4(v: *mut Variant, x: i32) {
    (*v).vt = VT_I4;
    (*v).val[0] = x as u32 as u64;
}

unsafe fn set_bool(v: *mut Variant, b: bool) {
    (*v).vt = VT_BOOL;
    (*v).val[0] = if b { 0xFFFF } else { 0 };
}

unsafe fn bstr(s: &str) -> *mut u16 {
    let w: Vec<u16> = s.encode_utf16().chain(Some(0)).collect();
    api().map_or(null_mut(), |a| (a.sys_alloc_string)(w.as_ptr()))
}

unsafe fn set_bstr(v: *mut Variant, s: &str) {
    (*v).vt = VT_BSTR;
    (*v).val[0] = bstr(s) as u64;
}

#[repr(C)]
struct UiaRect {
    left: f64,
    top: f64,
    width: f64,
    height: f64,
}

// ---- the provider object ----

type Unk = *mut c_void;

#[repr(C)]
struct Provider {
    simple: &'static SimpleVtbl,
    fragment: &'static FragmentVtbl,
    root: &'static RootVtbl,
    invoke: &'static InvokeVtbl,
    toggle: &'static ToggleVtbl,
    expand: &'static ExpandVtbl,
    selitem: &'static SelItemVtbl,
    value: &'static ValueVtbl,
    text: &'static TextVtbl,
    advise: &'static AdviseVtbl,
    refs: Cell<u32>,
    hwnd: HWND,
    src: &'static Source,
    path: Vec<usize>,
}

impl Provider {
    fn create(hwnd: HWND, src: &'static Source, path: Vec<usize>) -> *mut Provider {
        Box::into_raw(Box::new(Provider {
            simple: &SIMPLE,
            fragment: &FRAGMENT,
            root: &ROOT,
            invoke: &INVOKE,
            toggle: &TOGGLE,
            expand: &EXPAND,
            selitem: &SELITEM,
            value: &VALUE,
            text: &TEXT,
            advise: &ADVISE,
            refs: Cell::new(1),
            hwnd,
            src,
            path,
        }))
    }

    fn iface(p: *mut Provider, offset: usize) -> Unk {
        unsafe { (p as *mut u8).add(offset) as Unk }
    }

    unsafe fn from(this: Unk, offset: usize) -> &'static Provider {
        &*((this as *mut u8).sub(offset) as *const Provider)
    }

    unsafe fn release_raw(p: *mut Provider) {
        let n = (*p).refs.get() - 1;
        (*p).refs.set(n);
        if n == 0 {
            drop(Box::from_raw(p));
        }
    }

    /// The element's value: its own, or (for a text document) the first part of the
    /// text, read only when asked.
    fn value(&self, n: &Node) -> Option<String> {
        if n.value.is_some() {
            return n.value.clone();
        }
        let src = self.src.text.filter(|_| self.path.is_empty())?;
        let mut v = String::new();
        unsafe {
            (src.read)(self.hwnd, &mut |b| {
                v = b.chars_from(0).take(VALUE_CHARS).collect()
            })
        };
        Some(v)
    }

    fn node(&self) -> Option<Node> {
        let tree = unsafe { (self.src.tree)(self.hwnd) };
        tree.at(&self.path).cloned()
    }

    /// A new provider for another element of the same tree, as an interface pointer.
    fn sibling(&self, path: Vec<usize>, offset: usize) -> Unk {
        Provider::iface(Provider::create(self.hwnd, self.src, path), offset)
    }

    unsafe fn query(&self, riid: *const GUID, out: *mut Unk) -> HRESULT {
        if out.is_null() || riid.is_null() {
            return E_INVALIDARG;
        }
        let me = self as *const Provider as *mut Provider;
        let riid = &*riid;
        let offset = if same(riid, &IID_IUNKNOWN) || same(riid, &IID_SIMPLE) {
            offset_of!(Provider, simple)
        } else if same(riid, &IID_FRAGMENT) {
            offset_of!(Provider, fragment)
        } else if same(riid, &IID_ROOT) && self.path.is_empty() {
            offset_of!(Provider, root)
        } else if same(riid, &IID_INVOKE) {
            offset_of!(Provider, invoke)
        } else if same(riid, &IID_TOGGLE) {
            offset_of!(Provider, toggle)
        } else if same(riid, &IID_EXPAND) {
            offset_of!(Provider, expand)
        } else if same(riid, &IID_SELITEM) {
            offset_of!(Provider, selitem)
        } else if same(riid, &IID_VALUE) {
            offset_of!(Provider, value)
        } else if same(riid, &IID_TEXT) && self.path.is_empty() && self.src.text.is_some() {
            offset_of!(Provider, text)
        } else if same(riid, &IID_ADVISE) && self.path.is_empty() && self.src.text.is_some() {
            offset_of!(Provider, advise)
        } else {
            *out = null_mut();
            return E_NOINTERFACE;
        };
        self.refs.set(self.refs.get() + 1);
        *out = Provider::iface(me, offset);
        S_OK
    }

    fn screen_rect(&self, n: &Node) -> UiaRect {
        let mut pt = POINT {
            x: n.rect.x,
            y: n.rect.y,
        };
        unsafe { ClientToScreen(self.hwnd, &mut pt) };
        UiaRect {
            left: pt.x as f64,
            top: pt.y as f64,
            width: n.rect.w as f64,
            height: n.rect.h as f64,
        }
    }

    fn act(&self, a: Action) {
        unsafe { (self.src.act)(self.hwnd, a) }
    }
}

macro_rules! unknown {
    ($field:ident, $qi:ident, $add:ident, $rel:ident) => {
        unsafe extern "system" fn $qi(this: Unk, riid: *const GUID, out: *mut Unk) -> HRESULT {
            Provider::from(this, offset_of!(Provider, $field)).query(riid, out)
        }
        unsafe extern "system" fn $add(this: Unk) -> u32 {
            let p = Provider::from(this, offset_of!(Provider, $field));
            p.refs.set(p.refs.get() + 1);
            p.refs.get()
        }
        unsafe extern "system" fn $rel(this: Unk) -> u32 {
            let p = Provider::from(this, offset_of!(Provider, $field));
            let n = p.refs.get() - 1;
            Provider::release_raw(p as *const Provider as *mut Provider);
            n
        }
    };
}

/// Resolves `this` to its provider and node, or returns UIA_E_ELEMENTNOTAVAILABLE.
macro_rules! node {
    ($this:expr, $field:ident) => {{
        let p = Provider::from($this, offset_of!(Provider, $field));
        match p.node() {
            Some(n) => (p, n),
            None => return UIA_E_ELEMENTNOTAVAILABLE,
        }
    }};
}

// ---- IRawElementProviderSimple ----

#[repr(C)]
struct SimpleVtbl {
    qi: unsafe extern "system" fn(Unk, *const GUID, *mut Unk) -> HRESULT,
    add: unsafe extern "system" fn(Unk) -> u32,
    rel: unsafe extern "system" fn(Unk) -> u32,
    options: unsafe extern "system" fn(Unk, *mut i32) -> HRESULT,
    pattern: unsafe extern "system" fn(Unk, i32, *mut Unk) -> HRESULT,
    property: unsafe extern "system" fn(Unk, i32, *mut Variant) -> HRESULT,
    host: unsafe extern "system" fn(Unk, *mut Unk) -> HRESULT,
}

unknown!(simple, s_qi, s_add, s_rel);

unsafe extern "system" fn s_options(_: Unk, out: *mut i32) -> HRESULT {
    *out = PROVIDER_OPTIONS;
    S_OK
}

unsafe extern "system" fn s_pattern(this: Unk, id: i32, out: *mut Unk) -> HRESULT {
    *out = null_mut();
    let (p, n) = node!(this, simple);
    let offset = match id {
        PATTERN_INVOKE
            if matches!(n.action, Some(Action::Command(_) | Action::ActivateRow(_)))
                && n.checked.is_none() =>
        {
            offset_of!(Provider, invoke)
        }
        PATTERN_TOGGLE if n.checked.is_some() => offset_of!(Provider, toggle),
        PATTERN_EXPAND if n.expanded.is_some() => offset_of!(Provider, expand),
        PATTERN_SELITEM if n.selected.is_some() => offset_of!(Provider, selitem),
        PATTERN_VALUE if n.value.is_some() || (p.path.is_empty() && p.src.text.is_some()) => {
            offset_of!(Provider, value)
        }
        PATTERN_TEXT if p.path.is_empty() && p.src.text.is_some() => offset_of!(Provider, text),
        _ => return S_OK,
    };
    p.refs.set(p.refs.get() + 1);
    *out = Provider::iface(p as *const Provider as *mut Provider, offset);
    S_OK
}

unsafe extern "system" fn s_property(this: Unk, id: i32, out: *mut Variant) -> HRESULT {
    (*out).vt = VT_EMPTY;
    let (p, n) = node!(this, simple);
    let role = n.role.unwrap_or(Role::Text);
    let interactive = matches!(role, Role::MenuItem | Role::ListItem | Role::Document);
    match id {
        30003 => set_i4(out, control_type(role)),
        30005 => set_bstr(out, &n.name),
        30008 => set_bool(out, n.focused),
        30009 => set_bool(out, interactive),
        30010 => set_bool(out, true),
        30011 => {
            let path: Vec<String> = p.path.iter().map(|i| i.to_string()).collect();
            set_bstr(out, &format!("{}/{}", p.src.class, path.join(".")));
        }
        30012 => set_bstr(out, p.src.class),
        30016 => set_bool(out, true),
        30017 => set_bool(out, role != Role::Separator),
        30045 => set_bstr(out, &p.value(&n).unwrap_or_default()),
        30070 if n.expanded.is_some() => set_i4(out, if n.expanded == Some(true) { 1 } else { 0 }),
        30079 if n.selected.is_some() => set_bool(out, n.selected == Some(true)),
        30086 if n.checked.is_some() => set_i4(out, if n.checked == Some(true) { 1 } else { 0 }),
        _ => {}
    }
    S_OK
}

unsafe extern "system" fn s_host(this: Unk, out: *mut Unk) -> HRESULT {
    *out = null_mut();
    let p = Provider::from(this, offset_of!(Provider, simple));
    match (p.path.is_empty(), api()) {
        (true, Some(api)) => (api.host_from_hwnd)(p.hwnd, out),
        _ => S_OK,
    }
}

static SIMPLE: SimpleVtbl = SimpleVtbl {
    qi: s_qi,
    add: s_add,
    rel: s_rel,
    options: s_options,
    pattern: s_pattern,
    property: s_property,
    host: s_host,
};

// ---- IRawElementProviderFragment ----

#[repr(C)]
struct FragmentVtbl {
    qi: unsafe extern "system" fn(Unk, *const GUID, *mut Unk) -> HRESULT,
    add: unsafe extern "system" fn(Unk) -> u32,
    rel: unsafe extern "system" fn(Unk) -> u32,
    navigate: unsafe extern "system" fn(Unk, i32, *mut Unk) -> HRESULT,
    runtime_id: unsafe extern "system" fn(Unk, *mut *mut c_void) -> HRESULT,
    bounds: unsafe extern "system" fn(Unk, *mut UiaRect) -> HRESULT,
    embedded: unsafe extern "system" fn(Unk, *mut *mut c_void) -> HRESULT,
    set_focus: unsafe extern "system" fn(Unk) -> HRESULT,
    root: unsafe extern "system" fn(Unk, *mut Unk) -> HRESULT,
}

unknown!(fragment, f_qi, f_add, f_rel);

unsafe extern "system" fn f_navigate(this: Unk, dir: i32, out: *mut Unk) -> HRESULT {
    *out = null_mut();
    let (p, n) = node!(this, fragment);
    let frag = offset_of!(Provider, fragment);
    let tree = (p.src.tree)(p.hwnd);
    let target: Option<Vec<usize>> = match dir {
        // Parent
        0 => (!p.path.is_empty()).then(|| p.path[..p.path.len() - 1].to_vec()),
        // NextSibling / PreviousSibling
        1 | 2 if !p.path.is_empty() => {
            let (last, parent) = p.path.split_last().expect("non-empty");
            let siblings = tree.at(parent).map_or(0, |n| n.children.len());
            let i = if dir == 1 {
                last.checked_add(1)
            } else {
                last.checked_sub(1)
            };
            i.filter(|&i| i < siblings).map(|i| {
                let mut path = parent.to_vec();
                path.push(i);
                path
            })
        }
        // FirstChild / LastChild
        3 if !n.children.is_empty() => Some([p.path.as_slice(), &[0]].concat()),
        4 if !n.children.is_empty() => Some([p.path.as_slice(), &[n.children.len() - 1]].concat()),
        _ => None,
    };
    if let Some(path) = target {
        *out = p.sibling(path, frag);
    }
    S_OK
}

unsafe extern "system" fn f_runtime_id(this: Unk, out: *mut *mut c_void) -> HRESULT {
    *out = null_mut();
    let p = Provider::from(this, offset_of!(Provider, fragment));
    // The root is identified by its window; children append their path.
    if p.path.is_empty() {
        return S_OK;
    }
    let Some(api) = api() else { return S_OK };
    let ids: Vec<i32> = std::iter::once(APPEND_RUNTIME_ID)
        .chain(p.path.iter().map(|&i| i as i32 + 1))
        .collect();
    let sa = (api.sa_create_vector)(VT_I4, 0, ids.len() as u32);
    for (i, id) in ids.iter().enumerate() {
        let idx = i as i32;
        (api.sa_put)(sa, &idx, id as *const i32 as *const c_void);
    }
    *out = sa;
    S_OK
}

unsafe extern "system" fn f_bounds(this: Unk, out: *mut UiaRect) -> HRESULT {
    let (p, n) = node!(this, fragment);
    *out = p.screen_rect(&n);
    S_OK
}

unsafe extern "system" fn f_embedded(_: Unk, out: *mut *mut c_void) -> HRESULT {
    *out = null_mut();
    S_OK
}

unsafe extern "system" fn f_set_focus(_: Unk) -> HRESULT {
    S_OK
}

unsafe extern "system" fn f_root(this: Unk, out: *mut Unk) -> HRESULT {
    let p = Provider::from(this, offset_of!(Provider, fragment));
    *out = p.sibling(Vec::new(), offset_of!(Provider, root));
    S_OK
}

static FRAGMENT: FragmentVtbl = FragmentVtbl {
    qi: f_qi,
    add: f_add,
    rel: f_rel,
    navigate: f_navigate,
    runtime_id: f_runtime_id,
    bounds: f_bounds,
    embedded: f_embedded,
    set_focus: f_set_focus,
    root: f_root,
};

// ---- IRawElementProviderFragmentRoot ----

#[repr(C)]
struct RootVtbl {
    qi: unsafe extern "system" fn(Unk, *const GUID, *mut Unk) -> HRESULT,
    add: unsafe extern "system" fn(Unk) -> u32,
    rel: unsafe extern "system" fn(Unk) -> u32,
    from_point: unsafe extern "system" fn(Unk, f64, f64, *mut Unk) -> HRESULT,
    focus: unsafe extern "system" fn(Unk, *mut Unk) -> HRESULT,
}

unknown!(root, r_qi, r_add, r_rel);

unsafe extern "system" fn r_from_point(this: Unk, x: f64, y: f64, out: *mut Unk) -> HRESULT {
    *out = null_mut();
    let p = Provider::from(this, offset_of!(Provider, root));
    let mut pt = POINT {
        x: x as i32,
        y: y as i32,
    };
    ScreenToClient(p.hwnd, &mut pt);
    if let Some(path) = (p.src.tree)(p.hwnd).hit(pt.x, pt.y) {
        *out = p.sibling(path, offset_of!(Provider, fragment));
    }
    S_OK
}

unsafe extern "system" fn r_focus(this: Unk, out: *mut Unk) -> HRESULT {
    *out = null_mut();
    let p = Provider::from(this, offset_of!(Provider, root));
    if let Some(path) = (p.src.tree)(p.hwnd).focus_path().filter(|f| !f.is_empty()) {
        *out = p.sibling(path, offset_of!(Provider, fragment));
    }
    S_OK
}

static ROOT: RootVtbl = RootVtbl {
    qi: r_qi,
    add: r_add,
    rel: r_rel,
    from_point: r_from_point,
    focus: r_focus,
};

// ---- patterns ----

#[repr(C)]
struct InvokeVtbl {
    qi: unsafe extern "system" fn(Unk, *const GUID, *mut Unk) -> HRESULT,
    add: unsafe extern "system" fn(Unk) -> u32,
    rel: unsafe extern "system" fn(Unk) -> u32,
    invoke: unsafe extern "system" fn(Unk) -> HRESULT,
}

unknown!(invoke, i_qi, i_add, i_rel);

unsafe extern "system" fn i_invoke(this: Unk) -> HRESULT {
    let (p, n) = node!(this, invoke);
    match n.action {
        Some(a) => {
            p.act(a);
            S_OK
        }
        None => E_NOTIMPL,
    }
}

static INVOKE: InvokeVtbl = InvokeVtbl {
    qi: i_qi,
    add: i_add,
    rel: i_rel,
    invoke: i_invoke,
};

#[repr(C)]
struct ToggleVtbl {
    qi: unsafe extern "system" fn(Unk, *const GUID, *mut Unk) -> HRESULT,
    add: unsafe extern "system" fn(Unk) -> u32,
    rel: unsafe extern "system" fn(Unk) -> u32,
    toggle: unsafe extern "system" fn(Unk) -> HRESULT,
    state: unsafe extern "system" fn(Unk, *mut i32) -> HRESULT,
}

unknown!(toggle, t_qi, t_add, t_rel);

unsafe extern "system" fn t_toggle(this: Unk) -> HRESULT {
    i_invoke(Provider::iface(
        Provider::from(this, offset_of!(Provider, toggle)) as *const Provider as *mut Provider,
        offset_of!(Provider, invoke),
    ))
}

unsafe extern "system" fn t_state(this: Unk, out: *mut i32) -> HRESULT {
    let (_, n) = node!(this, toggle);
    *out = (n.checked == Some(true)) as i32;
    S_OK
}

static TOGGLE: ToggleVtbl = ToggleVtbl {
    qi: t_qi,
    add: t_add,
    rel: t_rel,
    toggle: t_toggle,
    state: t_state,
};

#[repr(C)]
struct ExpandVtbl {
    qi: unsafe extern "system" fn(Unk, *const GUID, *mut Unk) -> HRESULT,
    add: unsafe extern "system" fn(Unk) -> u32,
    rel: unsafe extern "system" fn(Unk) -> u32,
    expand: unsafe extern "system" fn(Unk) -> HRESULT,
    collapse: unsafe extern "system" fn(Unk) -> HRESULT,
    state: unsafe extern "system" fn(Unk, *mut i32) -> HRESULT,
}

unknown!(expand, e_qi, e_add, e_rel);

unsafe fn set_expanded(this: Unk, want: bool) -> HRESULT {
    let (p, n) = node!(this, expand);
    if n.expanded != Some(want) {
        if let Some(a) = n.action {
            p.act(a);
        }
    }
    S_OK
}

unsafe extern "system" fn e_expand(this: Unk) -> HRESULT {
    set_expanded(this, true)
}

unsafe extern "system" fn e_collapse(this: Unk) -> HRESULT {
    set_expanded(this, false)
}

unsafe extern "system" fn e_state(this: Unk, out: *mut i32) -> HRESULT {
    let (_, n) = node!(this, expand);
    *out = (n.expanded == Some(true)) as i32;
    S_OK
}

static EXPAND: ExpandVtbl = ExpandVtbl {
    qi: e_qi,
    add: e_add,
    rel: e_rel,
    expand: e_expand,
    collapse: e_collapse,
    state: e_state,
};

#[repr(C)]
struct SelItemVtbl {
    qi: unsafe extern "system" fn(Unk, *const GUID, *mut Unk) -> HRESULT,
    add: unsafe extern "system" fn(Unk) -> u32,
    rel: unsafe extern "system" fn(Unk) -> u32,
    select: unsafe extern "system" fn(Unk) -> HRESULT,
    add_to: unsafe extern "system" fn(Unk) -> HRESULT,
    remove_from: unsafe extern "system" fn(Unk) -> HRESULT,
    is_selected: unsafe extern "system" fn(Unk, *mut i32) -> HRESULT,
    container: unsafe extern "system" fn(Unk, *mut Unk) -> HRESULT,
}

unknown!(selitem, si_qi, si_add, si_rel);

unsafe extern "system" fn si_select(this: Unk) -> HRESULT {
    let (p, n) = node!(this, selitem);
    if let (Some(a), false) = (n.action, n.selected == Some(true)) {
        p.act(a);
    }
    S_OK
}

unsafe extern "system" fn si_add_to(this: Unk) -> HRESULT {
    si_select(this)
}

unsafe extern "system" fn si_remove(_: Unk) -> HRESULT {
    E_NOTIMPL
}

unsafe extern "system" fn si_is_selected(this: Unk, out: *mut i32) -> HRESULT {
    let (_, n) = node!(this, selitem);
    *out = (n.selected == Some(true)) as i32;
    S_OK
}

unsafe extern "system" fn si_container(this: Unk, out: *mut Unk) -> HRESULT {
    let p = Provider::from(this, offset_of!(Provider, selitem));
    *out = p.sibling(Vec::new(), offset_of!(Provider, simple));
    S_OK
}

static SELITEM: SelItemVtbl = SelItemVtbl {
    qi: si_qi,
    add: si_add,
    rel: si_rel,
    select: si_select,
    add_to: si_add_to,
    remove_from: si_remove,
    is_selected: si_is_selected,
    container: si_container,
};

#[repr(C)]
struct ValueVtbl {
    qi: unsafe extern "system" fn(Unk, *const GUID, *mut Unk) -> HRESULT,
    add: unsafe extern "system" fn(Unk) -> u32,
    rel: unsafe extern "system" fn(Unk) -> u32,
    set_value: unsafe extern "system" fn(Unk, *const u16) -> HRESULT,
    value: unsafe extern "system" fn(Unk, *mut *mut u16) -> HRESULT,
    read_only: unsafe extern "system" fn(Unk, *mut i32) -> HRESULT,
}

unknown!(value, v_qi, v_add, v_rel);

unsafe extern "system" fn v_set(_: Unk, _: *const u16) -> HRESULT {
    E_NOTIMPL
}

unsafe extern "system" fn v_value(this: Unk, out: *mut *mut u16) -> HRESULT {
    let (p, n) = node!(this, value);
    *out = bstr(&p.value(&n).unwrap_or_default());
    S_OK
}

unsafe extern "system" fn v_read_only(_: Unk, out: *mut i32) -> HRESULT {
    // Editing goes through the keyboard; the Value pattern only reads (the Text
    // pattern, #25 part 2, replaces it).
    *out = 1;
    S_OK
}

static VALUE: ValueVtbl = ValueVtbl {
    qi: v_qi,
    add: v_add,
    rel: v_rel,
    set_value: v_set,
    value: v_value,
    read_only: v_read_only,
};

// ---- Text pattern: ITextProvider on the document ----

#[repr(C)]
struct UiaPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
struct TextVtbl {
    qi: unsafe extern "system" fn(Unk, *const GUID, *mut Unk) -> HRESULT,
    add: unsafe extern "system" fn(Unk) -> u32,
    rel: unsafe extern "system" fn(Unk) -> u32,
    selection: unsafe extern "system" fn(Unk, *mut *mut c_void) -> HRESULT,
    visible: unsafe extern "system" fn(Unk, *mut *mut c_void) -> HRESULT,
    from_child: unsafe extern "system" fn(Unk, Unk, *mut Unk) -> HRESULT,
    from_point: unsafe extern "system" fn(Unk, UiaPoint, *mut Unk) -> HRESULT,
    document: unsafe extern "system" fn(Unk, *mut Unk) -> HRESULT,
    supported: unsafe extern "system" fn(Unk, *mut i32) -> HRESULT,
}

unknown!(text, tx_qi, tx_add, tx_rel);

fn text_src(p: &Provider) -> &'static TextSource {
    p.src.text.expect("text pattern only on text sources")
}

/// A one-element SAFEARRAY of IUnknown holding `range` (whose reference it takes).
unsafe fn range_array(range: Unk) -> *mut c_void {
    let Some(api) = api() else { return null_mut() };
    let sa = (api.sa_create_vector)(VT_UNKNOWN, 0, 1);
    let idx = 0i32;
    (api.sa_put)(sa, &idx, range);
    TextRange::release_raw(range as *mut TextRange);
    sa
}

unsafe extern "system" fn tx_selection(this: Unk, out: *mut *mut c_void) -> HRESULT {
    let p = Provider::from(this, offset_of!(Provider, text));
    let (s, e) = (text_src(p).selection)(p.hwnd);
    *out = range_array(TextRange::create(p.hwnd, p.src, s, e));
    S_OK
}

unsafe extern "system" fn tx_visible(this: Unk, out: *mut *mut c_void) -> HRESULT {
    let p = Provider::from(this, offset_of!(Provider, text));
    let (s, e) = (text_src(p).visible)(p.hwnd);
    *out = range_array(TextRange::create(p.hwnd, p.src, s, e));
    S_OK
}

unsafe extern "system" fn tx_from_child(_: Unk, _: Unk, out: *mut Unk) -> HRESULT {
    *out = null_mut();
    E_INVALIDARG
}

unsafe extern "system" fn tx_from_point(this: Unk, pt: UiaPoint, out: *mut Unk) -> HRESULT {
    let p = Provider::from(this, offset_of!(Provider, text));
    let mut cp = POINT {
        x: pt.x as i32,
        y: pt.y as i32,
    };
    ScreenToClient(p.hwnd, &mut cp);
    let at = (text_src(p).offset_at)(p.hwnd, cp.x, cp.y);
    *out = TextRange::create(p.hwnd, p.src, at, at);
    S_OK
}

unsafe extern "system" fn tx_document(this: Unk, out: *mut Unk) -> HRESULT {
    let p = Provider::from(this, offset_of!(Provider, text));
    let mut len = 0;
    (text_src(p).read)(p.hwnd, &mut |b| len = b.len());
    *out = TextRange::create(p.hwnd, p.src, 0, len);
    S_OK
}

unsafe extern "system" fn tx_supported(_: Unk, out: *mut i32) -> HRESULT {
    *out = 1; // SupportedTextSelection_Single
    S_OK
}

static TEXT: TextVtbl = TextVtbl {
    qi: tx_qi,
    add: tx_add,
    rel: tx_rel,
    selection: tx_selection,
    visible: tx_visible,
    from_child: tx_from_child,
    from_point: tx_from_point,
    document: tx_document,
    supported: tx_supported,
};

// ---- ITextRangeProvider ----

#[repr(C)]
struct TextRange {
    vtbl: &'static RangeVtbl,
    refs: Cell<u32>,
    hwnd: HWND,
    src: &'static Source,
    start: Cell<usize>,
    end: Cell<usize>,
}

impl TextRange {
    fn create(hwnd: HWND, src: &'static Source, start: usize, end: usize) -> Unk {
        Box::into_raw(Box::new(TextRange {
            vtbl: &RANGE,
            refs: Cell::new(1),
            hwnd,
            src,
            start: Cell::new(start.min(end)),
            end: Cell::new(start.max(end)),
        })) as Unk
    }

    unsafe fn this(p: Unk) -> &'static TextRange {
        &*(p as *const TextRange)
    }

    /// Another range passed back to us by UIA; only our own objects are accepted.
    unsafe fn other(p: Unk) -> Option<&'static TextRange> {
        let r = (p as *const TextRange).as_ref()?;
        std::ptr::eq(r.vtbl, &RANGE).then_some(r)
    }

    unsafe fn release_raw(p: *mut TextRange) {
        let n = (*p).refs.get() - 1;
        (*p).refs.set(n);
        if n == 0 {
            drop(Box::from_raw(p));
        }
    }

    fn src(&self) -> &'static TextSource {
        self.src.text.expect("range of a text source")
    }

    /// Runs `f` on the buffer with this range clamped to it.
    fn read<R: Default>(&self, mut f: impl FnMut(&Buffer, usize, usize) -> R) -> R {
        let mut out = R::default();
        let ok = unsafe {
            (self.src().read)(self.hwnd, &mut |b| {
                let len = b.len();
                let fix = |p: usize| {
                    let mut p = p.min(len);
                    while !b.is_char_boundary(p) {
                        p -= 1;
                    }
                    p
                };
                out = f(b, fix(self.start.get()), fix(self.end.get()));
            })
        };
        if !ok {
            return R::default();
        }
        out
    }

    fn set(&self, s: usize, e: usize) {
        self.start.set(s.min(e));
        self.end.set(s.max(e));
    }

    fn endpoint(&self, which: i32) -> usize {
        if which == 0 {
            self.start.get()
        } else {
            self.end.get()
        }
    }

    /// Moves one endpoint, keeping start <= end by collapsing the other one.
    fn set_endpoint(&self, which: i32, pos: usize) {
        if which == 0 {
            self.start.set(pos);
            if self.end.get() < pos {
                self.end.set(pos);
            }
        } else {
            self.end.set(pos);
            if self.start.get() > pos {
                self.start.set(pos);
            }
        }
    }
}

#[repr(C)]
struct RangeVtbl {
    qi: unsafe extern "system" fn(Unk, *const GUID, *mut Unk) -> HRESULT,
    add: unsafe extern "system" fn(Unk) -> u32,
    rel: unsafe extern "system" fn(Unk) -> u32,
    clone: unsafe extern "system" fn(Unk, *mut Unk) -> HRESULT,
    compare: unsafe extern "system" fn(Unk, Unk, *mut i32) -> HRESULT,
    compare_endpoints: unsafe extern "system" fn(Unk, i32, Unk, i32, *mut i32) -> HRESULT,
    expand: unsafe extern "system" fn(Unk, i32) -> HRESULT,
    find_attribute: unsafe extern "system" fn(Unk, i32, Variant, i32, *mut Unk) -> HRESULT,
    find_text: unsafe extern "system" fn(Unk, *const u16, i32, i32, *mut Unk) -> HRESULT,
    attribute: unsafe extern "system" fn(Unk, i32, *mut Variant) -> HRESULT,
    rects: unsafe extern "system" fn(Unk, *mut *mut c_void) -> HRESULT,
    enclosing: unsafe extern "system" fn(Unk, *mut Unk) -> HRESULT,
    get_text: unsafe extern "system" fn(Unk, i32, *mut *mut u16) -> HRESULT,
    move_: unsafe extern "system" fn(Unk, i32, i32, *mut i32) -> HRESULT,
    move_endpoint_by_unit: unsafe extern "system" fn(Unk, i32, i32, i32, *mut i32) -> HRESULT,
    move_endpoint_by_range: unsafe extern "system" fn(Unk, i32, Unk, i32) -> HRESULT,
    select: unsafe extern "system" fn(Unk) -> HRESULT,
    add_to_selection: unsafe extern "system" fn(Unk) -> HRESULT,
    remove_from_selection: unsafe extern "system" fn(Unk) -> HRESULT,
    scroll_into_view: unsafe extern "system" fn(Unk, i32) -> HRESULT,
    children: unsafe extern "system" fn(Unk, *mut *mut c_void) -> HRESULT,
}

unsafe extern "system" fn rg_qi(this: Unk, riid: *const GUID, out: *mut Unk) -> HRESULT {
    if out.is_null() || riid.is_null() {
        return E_INVALIDARG;
    }
    if same(&*riid, &IID_IUNKNOWN) || same(&*riid, &IID_RANGE) {
        rg_add(this);
        *out = this;
        S_OK
    } else {
        *out = null_mut();
        E_NOINTERFACE
    }
}

unsafe extern "system" fn rg_add(this: Unk) -> u32 {
    let r = TextRange::this(this);
    r.refs.set(r.refs.get() + 1);
    r.refs.get()
}

unsafe extern "system" fn rg_rel(this: Unk) -> u32 {
    let n = TextRange::this(this).refs.get() - 1;
    TextRange::release_raw(this as *mut TextRange);
    n
}

unsafe extern "system" fn rg_clone(this: Unk, out: *mut Unk) -> HRESULT {
    let r = TextRange::this(this);
    *out = TextRange::create(r.hwnd, r.src, r.start.get(), r.end.get());
    S_OK
}

unsafe extern "system" fn rg_compare(this: Unk, other: Unk, out: *mut i32) -> HRESULT {
    let r = TextRange::this(this);
    let Some(o) = TextRange::other(other) else {
        return E_INVALIDARG;
    };
    *out = (r.start.get() == o.start.get() && r.end.get() == o.end.get()) as i32;
    S_OK
}

unsafe extern "system" fn rg_compare_endpoints(
    this: Unk,
    ep: i32,
    other: Unk,
    oep: i32,
    out: *mut i32,
) -> HRESULT {
    let r = TextRange::this(this);
    let Some(o) = TextRange::other(other) else {
        return E_INVALIDARG;
    };
    let (a, b) = (r.endpoint(ep) as i64, o.endpoint(oep) as i64);
    *out = (a - b).clamp(-1, 1) as i32;
    S_OK
}

unsafe extern "system" fn rg_expand(this: Unk, unit: i32) -> HRESULT {
    let r = TextRange::this(this);
    let unit = Unit::from_uia(unit);
    let (s, e) = r.read(|b, s, _| {
        let start = textunits::unit_start(b, s, unit);
        (start, textunits::unit_end(b, start, unit))
    });
    r.set(s, e);
    S_OK
}

unsafe extern "system" fn rg_find_attribute(
    _: Unk,
    _: i32,
    _: Variant,
    _: i32,
    out: *mut Unk,
) -> HRESULT {
    // Plain text: no formatting attributes to match.
    *out = null_mut();
    S_OK
}

unsafe extern "system" fn rg_find_text(
    this: Unk,
    text: *const u16,
    backward: i32,
    ignore_case: i32,
    out: *mut Unk,
) -> HRESULT {
    *out = null_mut();
    let r = TextRange::this(this);
    if text.is_null() {
        return E_INVALIDARG;
    }
    let mut n = 0;
    while *text.add(n) != 0 {
        n += 1;
    }
    let needle = String::from_utf16_lossy(std::slice::from_raw_parts(text, n));
    let found = r.read(|b, s, e| {
        let m = if backward != 0 {
            b.find(&needle, e, false, ignore_case == 0)
        } else {
            b.find(&needle, s, true, ignore_case == 0)
        };
        m.filter(|m| m.start >= s && m.end <= e)
            .map(|m| (m.start, m.end))
    });
    if let Some((fs, fe)) = found {
        *out = TextRange::create(r.hwnd, r.src, fs, fe);
    }
    S_OK
}

unsafe extern "system" fn rg_attribute(_: Unk, attr: i32, out: *mut Variant) -> HRESULT {
    (*out).vt = VT_EMPTY;
    // UIA_IsReadOnlyAttributeId: editable. Everything else is unsupported.
    if attr == 40015 {
        set_bool(out, false);
        return S_OK;
    }
    if let Some(api) = api() {
        let mut unk: *mut c_void = null_mut();
        if (api.not_supported)(&mut unk) >= 0 {
            (*out).vt = VT_UNKNOWN;
            (*out).val[0] = unk as u64;
        }
    }
    S_OK
}

unsafe extern "system" fn rg_rects(this: Unk, out: *mut *mut c_void) -> HRESULT {
    *out = null_mut();
    let r = TextRange::this(this);
    let Some(api) = api() else { return S_OK };
    let rects = (r.src().rects)(r.hwnd, r.start.get(), r.end.get());
    let sa = (api.sa_create_vector)(VT_R8, 0, (rects.len() * 4) as u32);
    for (i, rc) in rects.iter().enumerate() {
        let mut pt = POINT { x: rc.x, y: rc.y };
        ClientToScreen(r.hwnd, &mut pt);
        for (j, v) in [pt.x as f64, pt.y as f64, rc.w as f64, rc.h as f64]
            .iter()
            .enumerate()
        {
            let idx = (i * 4 + j) as i32;
            (api.sa_put)(sa, &idx, v as *const f64 as *const c_void);
        }
    }
    *out = sa;
    S_OK
}

unsafe extern "system" fn rg_enclosing(this: Unk, out: *mut Unk) -> HRESULT {
    let r = TextRange::this(this);
    *out = Provider::iface(
        Provider::create(r.hwnd, r.src, Vec::new()),
        offset_of!(Provider, simple),
    );
    S_OK
}

unsafe extern "system" fn rg_get_text(this: Unk, max: i32, out: *mut *mut u16) -> HRESULT {
    let r = TextRange::this(this);
    let text = r.read(|b, s, e| b.slice(s..e));
    let text: String = if max >= 0 {
        text.chars().take(max as usize).collect()
    } else {
        text
    };
    *out = bstr(&text);
    S_OK
}

unsafe extern "system" fn rg_move(this: Unk, unit: i32, count: i32, out: *mut i32) -> HRESULT {
    let r = TextRange::this(this);
    let unit = Unit::from_uia(unit);
    let degenerate = r.start.get() == r.end.get();
    let (s, e, moved) = r.read(|b, s, _| {
        if count == 0 {
            return (s, s, 0);
        }
        let start = textunits::unit_start(b, s, unit);
        let (p, moved) = textunits::move_by(b, start, unit, count);
        let end = if degenerate {
            p
        } else {
            textunits::unit_end(b, p, unit)
        };
        (p, end, moved)
    });
    if count != 0 {
        r.set(s, e);
    }
    *out = moved;
    S_OK
}

unsafe extern "system" fn rg_move_endpoint_by_unit(
    this: Unk,
    ep: i32,
    unit: i32,
    count: i32,
    out: *mut i32,
) -> HRESULT {
    let r = TextRange::this(this);
    let unit = Unit::from_uia(unit);
    let at = r.endpoint(ep);
    let (p, moved) = r.read(|b, _, _| textunits::move_by(b, at, unit, count));
    r.set_endpoint(ep, p);
    *out = moved;
    S_OK
}

unsafe extern "system" fn rg_move_endpoint_by_range(
    this: Unk,
    ep: i32,
    other: Unk,
    oep: i32,
) -> HRESULT {
    let r = TextRange::this(this);
    let Some(o) = TextRange::other(other) else {
        return E_INVALIDARG;
    };
    r.set_endpoint(ep, o.endpoint(oep));
    S_OK
}

unsafe extern "system" fn rg_select(this: Unk) -> HRESULT {
    let r = TextRange::this(this);
    (r.src().select)(r.hwnd, r.start.get(), r.end.get());
    S_OK
}

unsafe extern "system" fn rg_add_to_selection(this: Unk) -> HRESULT {
    rg_select(this)
}

unsafe extern "system" fn rg_remove_from_selection(_: Unk) -> HRESULT {
    E_NOTIMPL
}

unsafe extern "system" fn rg_scroll_into_view(this: Unk, _align_top: i32) -> HRESULT {
    let r = TextRange::this(this);
    (r.src().scroll_to)(r.hwnd, r.start.get());
    S_OK
}

unsafe extern "system" fn rg_children(_: Unk, out: *mut *mut c_void) -> HRESULT {
    *out = api().map_or(null_mut(), |api| (api.sa_create_vector)(VT_UNKNOWN, 0, 0));
    S_OK
}

static RANGE: RangeVtbl = RangeVtbl {
    qi: rg_qi,
    add: rg_add,
    rel: rg_rel,
    clone: rg_clone,
    compare: rg_compare,
    compare_endpoints: rg_compare_endpoints,
    expand: rg_expand,
    find_attribute: rg_find_attribute,
    find_text: rg_find_text,
    attribute: rg_attribute,
    rects: rg_rects,
    enclosing: rg_enclosing,
    get_text: rg_get_text,
    move_: rg_move,
    move_endpoint_by_unit: rg_move_endpoint_by_unit,
    move_endpoint_by_range: rg_move_endpoint_by_range,
    select: rg_select,
    add_to_selection: rg_add_to_selection,
    remove_from_selection: rg_remove_from_selection,
    scroll_into_view: rg_scroll_into_view,
    children: rg_children,
};

// ---- IRawElementProviderAdviseEvents: who listens to text events ----

#[repr(C)]
struct AdviseVtbl {
    qi: unsafe extern "system" fn(Unk, *const GUID, *mut Unk) -> HRESULT,
    add: unsafe extern "system" fn(Unk) -> u32,
    rel: unsafe extern "system" fn(Unk) -> u32,
    added: unsafe extern "system" fn(Unk, i32, *mut c_void) -> HRESULT,
    removed: unsafe extern "system" fn(Unk, i32, *mut c_void) -> HRESULT,
}

unknown!(advise, ad_qi, ad_add, ad_rel);

fn is_text_event(id: i32) -> bool {
    id == TEXT_SELECTION_CHANGED_EVENT || id == TEXT_CHANGED_EVENT
}

unsafe extern "system" fn ad_added(_: Unk, event: i32, _: *mut c_void) -> HRESULT {
    if is_text_event(event) {
        TEXT_SUBSCRIBERS.fetch_add(1, Ordering::Relaxed);
    }
    S_OK
}

unsafe extern "system" fn ad_removed(_: Unk, event: i32, _: *mut c_void) -> HRESULT {
    if is_text_event(event) {
        TEXT_SUBSCRIBERS.fetch_sub(1, Ordering::Relaxed);
    }
    S_OK
}

static ADVISE: AdviseVtbl = AdviseVtbl {
    qi: ad_qi,
    add: ad_add,
    rel: ad_rel,
    added: ad_added,
    removed: ad_removed,
};
