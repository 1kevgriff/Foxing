//! The Windows folder picker (`IFileOpenDialog` with `FOS_PICKFOLDERS`), called through
//! its COM vtable directly so no COM wrapper crate is needed.

use std::ffi::c_void;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use std::ptr::{null, null_mut};
use windows_sys::core::{GUID, HRESULT};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};

const CLSID_FILE_OPEN_DIALOG: GUID = GUID::from_u128(0xdc1c5a9c_e88a_4dde_a5a1_60f82a20aef7);
const IID_IFILE_OPEN_DIALOG: GUID = GUID::from_u128(0xd57c7288_d4ad_4768_be02_9d969532d960);
const FOS_PICKFOLDERS: u32 = 0x20;
const FOS_FORCEFILESYSTEM: u32 = 0x40;
const SIGDN_FILESYSPATH: u32 = 0x8005_8000;

// Vtable slots (IUnknown: 0-2; IModalWindow::Show: 3; IFileDialog continues at 4).
const RELEASE: usize = 2;
const SHOW: usize = 3;
const SET_OPTIONS: usize = 9;
const GET_OPTIONS: usize = 10;
const GET_RESULT: usize = 20;
const ITEM_GET_DISPLAY_NAME: usize = 5;

unsafe fn slot(obj: *mut c_void, i: usize) -> *const c_void {
    let vtable = *(obj as *const *const *const c_void);
    *vtable.add(i)
}

unsafe fn release(obj: *mut c_void) {
    let f: unsafe extern "system" fn(*mut c_void) -> u32 = std::mem::transmute(slot(obj, RELEASE));
    f(obj);
}

/// Shows the folder picker; `None` if cancelled or unavailable.
pub unsafe fn pick_folder(owner: HWND) -> Option<PathBuf> {
    // Harmless if COM is already initialized on this thread.
    CoInitializeEx(null(), COINIT_APARTMENTTHREADED as u32);
    let mut dlg: *mut c_void = null_mut();
    let hr = CoCreateInstance(
        &CLSID_FILE_OPEN_DIALOG,
        null_mut(),
        CLSCTX_INPROC_SERVER,
        &IID_IFILE_OPEN_DIALOG,
        &mut dlg,
    );
    if hr < 0 || dlg.is_null() {
        return None;
    }
    let get_options: unsafe extern "system" fn(*mut c_void, *mut u32) -> HRESULT =
        std::mem::transmute(slot(dlg, GET_OPTIONS));
    let set_options: unsafe extern "system" fn(*mut c_void, u32) -> HRESULT =
        std::mem::transmute(slot(dlg, SET_OPTIONS));
    let show: unsafe extern "system" fn(*mut c_void, HWND) -> HRESULT =
        std::mem::transmute(slot(dlg, SHOW));
    let get_result: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT =
        std::mem::transmute(slot(dlg, GET_RESULT));

    let mut opts = 0u32;
    get_options(dlg, &mut opts);
    set_options(dlg, opts | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM);
    let mut path = None;
    if show(dlg, owner) >= 0 {
        let mut item: *mut c_void = null_mut();
        if get_result(dlg, &mut item) >= 0 && !item.is_null() {
            let display: unsafe extern "system" fn(*mut c_void, u32, *mut *mut u16) -> HRESULT =
                std::mem::transmute(slot(item, ITEM_GET_DISPLAY_NAME));
            let mut name: *mut u16 = null_mut();
            if display(item, SIGDN_FILESYSPATH, &mut name) >= 0 && !name.is_null() {
                let mut n = 0;
                while *name.add(n) != 0 {
                    n += 1;
                }
                let wide = std::slice::from_raw_parts(name, n);
                path = Some(PathBuf::from(std::ffi::OsString::from_wide(wide)));
                CoTaskMemFree(name as *const c_void);
            }
            release(item);
        }
    }
    release(dlg);
    path
}
