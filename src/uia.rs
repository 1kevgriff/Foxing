//! UI Automation provider for Foxing's custom components. Each host window exposes an
//! accessibility tree ([`Node`]) through one generic COM object per element, built
//! from raw vtables (no COM crate). UIAutomationCore and oleaut32 are loaded on the
//! first `WM_GETOBJECT`, so there is no startup cost when no screen reader is running.

use foxing::ui::a11y::{Action, Node, Role};
use std::cell::Cell;
use std::ffi::c_void;
use std::mem::offset_of;
use std::ptr::null_mut;
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

const VT_EMPTY: u16 = 0;
const VT_I4: u16 = 3;
const VT_BSTR: u16 = 8;
const VT_BOOL: u16 = 11;

const IID_IUNKNOWN: GUID = GUID::from_u128(0x00000000_0000_0000_c000_000000000046);
const IID_SIMPLE: GUID = GUID::from_u128(0xd6dd68d1_86fd_4332_8666_9abedea2d24c);
const IID_FRAGMENT: GUID = GUID::from_u128(0xf7063da8_8359_439c_9297_bbc5299a7d87);
const IID_ROOT: GUID = GUID::from_u128(0x620ce2a5_ab8f_40a9_86cb_de3c75599b58);
const IID_INVOKE: GUID = GUID::from_u128(0x54fcb24b_e18e_47a2_b4d3_eccbe77599a2);
const IID_TOGGLE: GUID = GUID::from_u128(0x56d00bd0_c4f4_433c_a836_1a52a57e0892);
const IID_EXPAND: GUID = GUID::from_u128(0xd847d3a5_cab0_4a98_8c32_ecb45c59ad24);
const IID_SELITEM: GUID = GUID::from_u128(0x2acad808_b2d4_452d_a407_91ff1ad167b2);
const IID_VALUE: GUID = GUID::from_u128(0xc7935180_6fb3_4201_b174_7df73adbf64a);

const PATTERN_INVOKE: i32 = 10000;
const PATTERN_VALUE: i32 = 10002;
const PATTERN_EXPAND: i32 = 10005;
const PATTERN_SELITEM: i32 = 10010;
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

struct Api {
    return_provider: FnReturnProvider,
    host_from_hwnd: FnHostFromHwnd,
    raise_event: FnRaiseEvent,
    clients_listening: FnClientsListening,
    sys_alloc_string: FnSysAllocString,
    sa_create_vector: FnSaCreateVector,
    sa_put: FnSaPut,
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
        PATTERN_VALUE if n.value.is_some() => offset_of!(Provider, value),
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
        30045 => set_bstr(out, n.value.as_deref().unwrap_or("")),
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
    let (_, n) = node!(this, value);
    *out = bstr(n.value.as_deref().unwrap_or(""));
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
