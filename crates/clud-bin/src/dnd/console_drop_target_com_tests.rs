//! Windows-only tests for the COM layer Explorer calls into during a
//! real drag (#1362): the `IDropTarget` vtable built by
//! `win::new_drop_target`, and `win::copy_cf_hdrop_bytes` pulling the
//! `CF_HDROP` `HGLOBAL` out of an `IDataObject`.
//!
//! Every call goes through the real vtable pointers. The drag source is
//! `FakeDataObject`, an in-process `IDataObject` that serves an
//! `STGMEDIUM` built here, so no console window, `OleInitialize` or
//! `RegisterDragDrop` is needed and the tests run on a headless CI
//! runner. The `RegisterDragDrop` / `RevokeDragDrop` round trip itself
//! still needs a real console window; see the dnd README.

use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use windows::core::{implement, IUnknown, Interface, HRESULT};
use windows::Win32::Foundation::{
    GlobalFree, DV_E_FORMATETC, E_NOINTERFACE, E_NOTIMPL, HGLOBAL, OLE_E_ADVISENOTSUPPORTED,
    POINTL, S_FALSE, S_OK,
};
use windows::Win32::System::Com::{
    IAdviseSink, IDataObject, IDataObject_Impl, IEnumFORMATETC, IEnumSTATDATA, IStream,
    DVASPECT_CONTENT, FORMATETC, STGMEDIUM, STGMEDIUM_0, TYMED_HGLOBAL, TYMED_ISTREAM,
};
use windows::Win32::System::Memory::{
    GlobalAlloc, GlobalFlags, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE,
};
use windows::Win32::System::Ole::{
    IDropTarget, CF_HDROP, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_MOVE,
    DROPEFFECT_NONE,
};
use windows::Win32::System::SystemServices::MK_LBUTTON;
use windows::Win32::UI::Shell::SHCreateMemStream;

use super::tests::make_dropfiles_wide;
use super::win::{copy_cf_hdrop_bytes, new_drop_target};
use super::{DropInjector, DROPEFFECT_COPY_BITS, DROPEFFECT_NONE_BITS};

/// What Explorer offers: every effect.
const ANY_EFFECT: DROPEFFECT =
    DROPEFFECT(DROPEFFECT_COPY.0 | DROPEFFECT_MOVE.0 | DROPEFFECT_LINK.0);

/// `GMEM_LOCKCOUNT` (winbase.h): the lock-count bits of `GlobalFlags`.
const GMEM_LOCKCOUNT: u32 = 0x00FF;

const POINT: POINTL = POINTL { x: 10, y: 20 };

// ─── Fake drag source ─────────────────────────────────────────────────

/// The medium `FakeDataObject::GetData` hands back.
enum Medium {
    /// `CF_HDROP` bytes in a fresh `HGLOBAL` with a null
    /// `pUnkForRelease`, so `ReleaseStgMedium` frees it. This is the
    /// shape Explorer's data object returns.
    OwnedHglobal(Vec<u8>),
    /// A test-owned `HGLOBAL` with `pUnkForRelease` set to `token`:
    /// `ReleaseStgMedium` then releases the token instead of freeing
    /// the memory, so the test can count the release and inspect the
    /// lock count afterwards.
    SharedHglobal { hglobal: HGLOBAL, token: IUnknown },
    /// A stream medium, which clud does not read but must release.
    Stream(IStream),
    /// `GetData` fails with `DV_E_FORMATETC`.
    Refuse,
}

/// One `FORMATETC` a caller asked the fake for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Request {
    cf_format: u16,
    aspect: u32,
    lindex: i32,
    tymed: u32,
    null_ptd: bool,
}

impl Request {
    fn of(format: &FORMATETC) -> Self {
        Self {
            cf_format: format.cfFormat,
            aspect: format.dwAspect,
            lindex: format.lindex,
            tymed: format.tymed,
            null_ptd: format.ptd.is_null(),
        }
    }

    /// The only request a `CF_HDROP` source should answer.
    fn is_cf_hdrop_hglobal(self) -> bool {
        self.cf_format == CF_HDROP.0
            && self.aspect == DVASPECT_CONTENT.0
            && self.lindex == -1
            && self.tymed & TYMED_HGLOBAL.0 as u32 != 0
            && self.null_ptd
    }
}

#[implement(IDataObject)]
struct FakeDataObject {
    medium: Medium,
    /// What `QueryGetData` answers for a `CF_HDROP` request.
    query_answer: HRESULT,
    requests: Arc<Mutex<Vec<Request>>>,
    get_data_calls: Arc<AtomicUsize>,
}

/// Handles for inspecting a `FakeDataObject` after it is converted to
/// an `IDataObject`.
struct Source {
    data: IDataObject,
    requests: Arc<Mutex<Vec<Request>>>,
    get_data_calls: Arc<AtomicUsize>,
}

fn source(medium: Medium, query_answer: HRESULT) -> Source {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let get_data_calls = Arc::new(AtomicUsize::new(0));
    let data: IDataObject = FakeDataObject {
        medium,
        query_answer,
        requests: Arc::clone(&requests),
        get_data_calls: Arc::clone(&get_data_calls),
    }
    .into();
    Source {
        data,
        requests,
        get_data_calls,
    }
}

/// An Explorer-shaped file drag.
fn file_source(paths: &[&str]) -> Source {
    source(Medium::OwnedHglobal(make_dropfiles_wide(paths)), S_OK)
}

/// A drag that carries no files (text, an image, a URL): `QueryGetData`
/// and `GetData` both refuse `CF_HDROP`.
fn non_file_source() -> Source {
    source(Medium::Refuse, DV_E_FORMATETC)
}

/// Copy `bytes` into a new moveable `HGLOBAL`.
fn hglobal_with(bytes: &[u8]) -> HGLOBAL {
    // SAFETY: plain allocation; the lock is paired with the unlock below.
    unsafe {
        let hglobal = GlobalAlloc(GMEM_MOVEABLE, bytes.len()).expect("GlobalAlloc");
        let dst = GlobalLock(hglobal) as *mut u8;
        assert!(!dst.is_null(), "GlobalLock failed on a fresh HGLOBAL");
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst, bytes.len());
        let _ = GlobalUnlock(hglobal);
        hglobal
    }
}

#[allow(non_snake_case)]
impl IDataObject_Impl for FakeDataObject_Impl {
    fn GetData(&self, format: *const FORMATETC) -> windows_core::Result<STGMEDIUM> {
        self.get_data_calls.fetch_add(1, Ordering::SeqCst);
        // SAFETY: OLE passes a valid FORMATETC.
        let request = Request::of(unsafe { &*format });
        self.requests.lock().unwrap().push(request);
        if !request.is_cf_hdrop_hglobal() {
            return Err(DV_E_FORMATETC.into());
        }
        match &self.medium {
            Medium::OwnedHglobal(bytes) => Ok(STGMEDIUM {
                tymed: TYMED_HGLOBAL.0 as u32,
                u: STGMEDIUM_0 {
                    hGlobal: hglobal_with(bytes),
                },
                pUnkForRelease: ManuallyDrop::new(None),
            }),
            Medium::SharedHglobal { hglobal, token } => Ok(STGMEDIUM {
                tymed: TYMED_HGLOBAL.0 as u32,
                u: STGMEDIUM_0 { hGlobal: *hglobal },
                pUnkForRelease: ManuallyDrop::new(Some(token.clone())),
            }),
            Medium::Stream(stream) => Ok(STGMEDIUM {
                tymed: TYMED_ISTREAM.0 as u32,
                u: STGMEDIUM_0 {
                    pstm: ManuallyDrop::new(Some(stream.clone())),
                },
                pUnkForRelease: ManuallyDrop::new(None),
            }),
            Medium::Refuse => Err(DV_E_FORMATETC.into()),
        }
    }

    fn GetDataHere(
        &self,
        _format: *const FORMATETC,
        _medium: *mut STGMEDIUM,
    ) -> windows_core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn QueryGetData(&self, format: *const FORMATETC) -> HRESULT {
        // SAFETY: OLE passes a valid FORMATETC.
        let request = Request::of(unsafe { &*format });
        self.requests.lock().unwrap().push(request);
        if request.is_cf_hdrop_hglobal() {
            self.query_answer
        } else {
            DV_E_FORMATETC
        }
    }

    fn GetCanonicalFormatEtc(&self, _in: *const FORMATETC, _out: *mut FORMATETC) -> HRESULT {
        E_NOTIMPL
    }

    fn SetData(
        &self,
        _format: *const FORMATETC,
        _medium: *const STGMEDIUM,
        _release: windows_core::BOOL,
    ) -> windows_core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn EnumFormatEtc(&self, _direction: u32) -> windows_core::Result<IEnumFORMATETC> {
        Err(E_NOTIMPL.into())
    }

    fn DAdvise(
        &self,
        _format: *const FORMATETC,
        _advf: u32,
        _sink: windows_core::Ref<'_, IAdviseSink>,
    ) -> windows_core::Result<u32> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn DUnadvise(&self, _connection: u32) -> windows_core::Result<()> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn EnumDAdvise(&self) -> windows_core::Result<IEnumSTATDATA> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────

/// The object's reference count, read through the raw `AddRef` /
/// `Release` vtable slots (each returns the new count).
fn ref_count(unknown: &IUnknown) -> u32 {
    let vtable = unknown.vtable();
    let raw = unknown.as_raw();
    // SAFETY: balanced AddRef/Release on a live interface.
    unsafe {
        let after_add = (vtable.AddRef)(raw);
        let after_release = (vtable.Release)(raw);
        assert_eq!(after_release + 1, after_add);
        after_release
    }
}

fn memory_stream() -> IStream {
    // SAFETY: no preconditions; returns None only on allocation failure.
    unsafe { SHCreateMemStream(Some(b"not a DROPFILES payload".as_slice())) }
        .expect("SHCreateMemStream")
}

/// An injector that records every delivered path list.
fn recording_injector() -> (DropInjector, Arc<Mutex<Vec<Vec<String>>>>) {
    let drops = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&drops);
    let injector: DropInjector = Box::new(move |paths: &[String]| {
        sink.lock().unwrap().push(paths.to_vec());
    });
    (injector, drops)
}

fn drag_enter(target: &IDropTarget, data: Option<&IDataObject>, allowed: DROPEFFECT) -> DROPEFFECT {
    let mut effect = allowed;
    // SAFETY: calls the real vtable slot with a valid out-pointer.
    unsafe { target.DragEnter(data, MK_LBUTTON, POINT, &mut effect) }
        .expect("DragEnter must return S_OK");
    effect
}

fn drag_over(target: &IDropTarget, allowed: DROPEFFECT) -> DROPEFFECT {
    let mut effect = allowed;
    // SAFETY: calls the real vtable slot with a valid out-pointer.
    unsafe { target.DragOver(MK_LBUTTON, POINT, &mut effect) }.expect("DragOver must return S_OK");
    effect
}

fn drop_on(target: &IDropTarget, data: Option<&IDataObject>, allowed: DROPEFFECT) -> DROPEFFECT {
    let mut effect = allowed;
    // SAFETY: calls the real vtable slot with a valid out-pointer.
    unsafe { target.Drop(data, MK_LBUTTON, POINT, &mut effect) }.expect("Drop must return S_OK");
    effect
}

// ─── Pure decision pinned to the Win32 constants ──────────────────────

#[test]
fn dropeffect_bits_match_the_win32_constants() {
    assert_eq!(DROPEFFECT_NONE_BITS, DROPEFFECT_NONE.0);
    assert_eq!(DROPEFFECT_COPY_BITS, DROPEFFECT_COPY.0);
}

// ─── IUnknown: QueryInterface / AddRef / Release ──────────────────────

#[test]
fn query_interface_answers_iunknown_and_idroptarget_only() {
    let target = new_drop_target(Box::new(|_| {}));

    let unknown: IUnknown = target.cast().expect("QueryInterface(IUnknown)");
    let again: IDropTarget = unknown.cast().expect("QueryInterface(IDropTarget)");
    assert_eq!(again.as_raw(), target.as_raw());
    let unknown_again: IUnknown = again.cast().expect("QueryInterface(IUnknown)");
    assert_eq!(
        unknown_again.as_raw(),
        unknown.as_raw(),
        "COM identity: every IUnknown query returns the same pointer"
    );

    let err = target
        .cast::<IDataObject>()
        .expect_err("the drop target must not claim IDataObject");
    assert_eq!(err.code(), E_NOINTERFACE);
}

/// Sets its flag when dropped, so a test can see the injector freed.
struct DropSignal(Arc<AtomicBool>);

impl Drop for DropSignal {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[test]
fn addref_release_track_the_count_and_the_last_release_frees_the_target() {
    let freed = Arc::new(AtomicBool::new(false));
    let signal = DropSignal(Arc::clone(&freed));
    let target = new_drop_target(Box::new(move |_: &[String]| {
        std::hint::black_box(&signal);
    }));

    let unknown: IUnknown = target.cast().unwrap();
    assert_eq!(ref_count(&unknown), 2, "target + IUnknown");

    let vtable = unknown.vtable();
    // SAFETY: balanced AddRef/Release on a live interface.
    unsafe {
        assert_eq!((vtable.AddRef)(unknown.as_raw()), 3);
        assert_eq!((vtable.Release)(unknown.as_raw()), 2);
    }

    drop(unknown);
    assert!(!freed.load(Ordering::SeqCst), "one reference is still held");
    drop(target);
    assert!(
        freed.load(Ordering::SeqCst),
        "the last Release must destroy the target and its injector"
    );
}

#[test]
fn a_drag_and_drop_leaves_no_reference_on_the_target_or_the_source() {
    let (injector, _drops) = recording_injector();
    let target = new_drop_target(injector);
    let src = file_source(&[r"C:\a.txt"]);
    let target_unknown: IUnknown = target.cast().unwrap();
    let data_unknown: IUnknown = src.data.cast().unwrap();
    assert_eq!(ref_count(&target_unknown), 2);
    assert_eq!(ref_count(&data_unknown), 2);

    drag_enter(&target, Some(&src.data), ANY_EFFECT);
    drag_over(&target, ANY_EFFECT);
    drop_on(&target, Some(&src.data), ANY_EFFECT);

    assert_eq!(ref_count(&target_unknown), 2, "target reference leaked");
    assert_eq!(
        ref_count(&data_unknown),
        2,
        "the target must not keep the IDataObject past the drag"
    );
}

// ─── DragEnter / DragOver / DragLeave ─────────────────────────────────

#[test]
fn a_file_drag_advertises_copy_on_enter_and_over() {
    let target = new_drop_target(Box::new(|_| {}));
    let src = file_source(&[r"C:\a.txt"]);

    assert_eq!(
        drag_enter(&target, Some(&src.data), ANY_EFFECT),
        DROPEFFECT_COPY
    );
    assert_eq!(drag_over(&target, ANY_EFFECT), DROPEFFECT_COPY);

    let requests = src.requests.lock().unwrap().clone();
    assert!(
        !requests.is_empty() && requests.iter().all(|r| r.is_cf_hdrop_hglobal()),
        "DragEnter must ask for CF_HDROP in an HGLOBAL, got {requests:?}"
    );
    assert_eq!(
        src.get_data_calls.load(Ordering::SeqCst),
        0,
        "DragEnter must only query the format, not render the payload"
    );
}

#[test]
fn a_drag_without_files_shows_the_no_drop_cursor() {
    let target = new_drop_target(Box::new(|_| {}));
    let src = non_file_source();

    assert_eq!(
        drag_enter(&target, Some(&src.data), ANY_EFFECT),
        DROPEFFECT_NONE
    );
    assert_eq!(drag_over(&target, ANY_EFFECT), DROPEFFECT_NONE);
}

#[test]
fn only_s_ok_from_query_get_data_counts_as_carrying_files() {
    let target = new_drop_target(Box::new(|_| {}));
    let src = source(Medium::Refuse, S_FALSE);

    assert_eq!(
        drag_enter(&target, Some(&src.data), ANY_EFFECT),
        DROPEFFECT_NONE
    );
}

#[test]
fn a_null_data_object_on_enter_is_refused() {
    let target = new_drop_target(Box::new(|_| {}));
    assert_eq!(drag_enter(&target, None, ANY_EFFECT), DROPEFFECT_NONE);
}

#[test]
fn a_source_that_forbids_copy_is_refused_even_with_files() {
    // clud only reads the paths; claiming MOVE would make Explorer
    // delete the dragged file.
    let target = new_drop_target(Box::new(|_| {}));
    let src = file_source(&[r"C:\a.txt"]);

    assert_eq!(
        drag_enter(&target, Some(&src.data), DROPEFFECT_MOVE),
        DROPEFFECT_NONE
    );
    assert_eq!(drag_over(&target, DROPEFFECT_MOVE), DROPEFFECT_NONE);
    // The allowed set can change mid-drag (modifier keys); DragOver
    // follows it.
    assert_eq!(drag_over(&target, ANY_EFFECT), DROPEFFECT_COPY);
}

#[test]
fn drag_leave_ends_the_file_drag() {
    let target = new_drop_target(Box::new(|_| {}));
    let files = file_source(&[r"C:\a.txt"]);
    let text = non_file_source();

    assert_eq!(
        drag_enter(&target, Some(&files.data), ANY_EFFECT),
        DROPEFFECT_COPY
    );
    // SAFETY: real vtable slot, no arguments.
    unsafe { target.DragLeave() }.expect("DragLeave must return S_OK");
    assert_eq!(
        drag_enter(&target, Some(&text.data), ANY_EFFECT),
        DROPEFFECT_NONE
    );
    assert_eq!(drag_over(&target, ANY_EFFECT), DROPEFFECT_NONE);
}

#[test]
fn a_null_effect_pointer_is_tolerated() {
    let (injector, drops) = recording_injector();
    let target = new_drop_target(injector);
    let src = file_source(&[r"C:\a.txt"]);
    let raw = target.as_raw();
    let vtable = target.vtable();
    let data = src.data.as_raw();
    // SAFETY: the real vtable slots; a null effect pointer violates the
    // contract, and the target must neither write through it nor fail.
    unsafe {
        assert_eq!(
            (vtable.DragEnter)(raw, data, MK_LBUTTON, POINT, std::ptr::null_mut()),
            S_OK
        );
        assert_eq!(
            (vtable.DragOver)(raw, MK_LBUTTON, POINT, std::ptr::null_mut()),
            S_OK
        );
        assert_eq!(
            (vtable.Drop)(raw, data, MK_LBUTTON, POINT, std::ptr::null_mut()),
            S_OK
        );
    }
    assert_eq!(drops.lock().unwrap().len(), 1);
}

// ─── Drop ─────────────────────────────────────────────────────────────

#[test]
fn drop_delivers_the_paths_and_reports_copy() {
    let (injector, drops) = recording_injector();
    let target = new_drop_target(injector);
    let src = file_source(&[r"C:\test\a.txt", r"C:\Users\me\Документы\b c.txt"]);

    drag_enter(&target, Some(&src.data), ANY_EFFECT);
    assert_eq!(
        drop_on(&target, Some(&src.data), ANY_EFFECT),
        DROPEFFECT_COPY
    );

    assert_eq!(
        *drops.lock().unwrap(),
        vec![vec![
            r"C:\test\a.txt".to_string(),
            r"C:\Users\me\Документы\b c.txt".to_string()
        ]]
    );
    assert_eq!(src.get_data_calls.load(Ordering::SeqCst), 1);
    let requests = src.requests.lock().unwrap().clone();
    assert!(
        requests.iter().all(|r| r.is_cf_hdrop_hglobal()),
        "Drop must ask for CF_HDROP in an HGLOBAL, got {requests:?}"
    );
}

#[test]
fn drop_without_files_reports_none_and_injects_nothing() {
    let (injector, drops) = recording_injector();
    let target = new_drop_target(injector);
    let src = non_file_source();

    assert_eq!(
        drop_on(&target, Some(&src.data), ANY_EFFECT),
        DROPEFFECT_NONE
    );
    assert_eq!(drop_on(&target, None, ANY_EFFECT), DROPEFFECT_NONE);
    assert!(drops.lock().unwrap().is_empty());
}

#[test]
fn drop_from_a_source_that_forbids_copy_injects_nothing() {
    let (injector, drops) = recording_injector();
    let target = new_drop_target(injector);
    let src = file_source(&[r"C:\a.txt"]);

    assert_eq!(
        drop_on(&target, Some(&src.data), DROPEFFECT_MOVE),
        DROPEFFECT_NONE
    );
    assert!(drops.lock().unwrap().is_empty());
    assert_eq!(
        src.get_data_calls.load(Ordering::SeqCst),
        0,
        "a refused drop must not render the payload"
    );
}

#[test]
fn drop_with_a_malformed_cf_hdrop_reports_none() {
    // A CF_HDROP medium whose DROPFILES header is truncated: GetData
    // succeeds, but no path can be parsed out of it.
    let (injector, drops) = recording_injector();
    let target = new_drop_target(injector);
    let src = source(Medium::OwnedHglobal(vec![0u8; 8]), S_OK);

    assert_eq!(
        drop_on(&target, Some(&src.data), ANY_EFFECT),
        DROPEFFECT_NONE
    );
    assert!(drops.lock().unwrap().is_empty());
}

#[test]
fn a_panicking_injector_does_not_unwind_across_the_vtable_or_poison_the_next_drop() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&calls);
    let target = new_drop_target(Box::new(move |_: &[String]| {
        if seen.fetch_add(1, Ordering::SeqCst) == 0 {
            panic!("injector failure (expected by this test)");
        }
    }));
    let src = file_source(&[r"C:\a.txt"]);

    assert_eq!(
        drop_on(&target, Some(&src.data), ANY_EFFECT),
        DROPEFFECT_NONE,
        "a drop whose injector panicked delivered nothing"
    );
    assert_eq!(
        drop_on(&target, Some(&src.data), ANY_EFFECT),
        DROPEFFECT_COPY
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

// ─── copy_cf_hdrop_bytes: GetData / GlobalLock / ReleaseStgMedium ─────

#[test]
fn copy_reads_the_whole_hglobal_unlocks_it_and_releases_the_medium_once() {
    let payload = make_dropfiles_wide(&[r"C:\a.txt", r"D:\b.txt"]);
    let hglobal = hglobal_with(&payload);
    let token: IUnknown = memory_stream().cast().unwrap();
    let held = token.clone();
    assert_eq!(ref_count(&held), 2);
    // SAFETY: `hglobal` is live until the GlobalFree below.
    let size = unsafe { GlobalSize(hglobal) };
    assert!(size >= payload.len());

    let src = source(
        Medium::SharedHglobal {
            hglobal,
            token: token.clone(),
        },
        S_OK,
    );
    assert_eq!(ref_count(&held), 3, "the fake holds one token reference");

    // SAFETY: `src.data` is a live IDataObject.
    let bytes = unsafe { copy_cf_hdrop_bytes(&src.data) }.expect("a CF_HDROP HGLOBAL medium");

    assert_eq!(bytes.len(), size, "the copy must span GlobalSize bytes");
    assert_eq!(&bytes[..payload.len()], payload.as_slice());
    assert_eq!(
        ref_count(&held),
        3,
        "ReleaseStgMedium must release pUnkForRelease exactly once \
         (4 = leaked, 2 = released twice)"
    );
    // SAFETY: still live; reads the flags only.
    let lock_count = unsafe { GlobalFlags(hglobal) } & GMEM_LOCKCOUNT;
    assert_eq!(lock_count, 0, "GlobalLock must be paired with GlobalUnlock");

    drop(src);
    drop(token);
    // SAFETY: the test owns this HGLOBAL; nothing else frees it. The
    // `windows` wrapper maps GlobalFree's NULL success return to `Err`,
    // so the result carries no signal.
    let _ = unsafe { GlobalFree(Some(hglobal)) };
}

#[test]
fn copy_of_an_explorer_shaped_medium_round_trips_the_payload() {
    let payload = make_dropfiles_wide(&[r"C:\one.txt"]);
    let src = source(Medium::OwnedHglobal(payload.clone()), S_OK);

    // SAFETY: `src.data` is a live IDataObject. The medium's HGLOBAL
    // has a null pUnkForRelease, so ReleaseStgMedium frees it.
    let bytes = unsafe { copy_cf_hdrop_bytes(&src.data) }.expect("a CF_HDROP HGLOBAL medium");
    assert_eq!(&bytes[..payload.len()], payload.as_slice());
    assert_eq!(src.get_data_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn copy_rejects_a_non_hglobal_medium_and_still_releases_it() {
    let stream = memory_stream();
    let held: IUnknown = stream.cast().unwrap();
    let src = source(Medium::Stream(stream), S_OK);
    let before = ref_count(&held);

    // SAFETY: `src.data` is a live IDataObject.
    let bytes = unsafe { copy_cf_hdrop_bytes(&src.data) };

    assert!(bytes.is_none(), "a TYMED_ISTREAM medium is not CF_HDROP");
    assert_eq!(
        ref_count(&held),
        before,
        "the stream GetData handed out must be released"
    );
}

#[test]
fn copy_returns_none_when_get_data_fails() {
    let src = non_file_source();
    // SAFETY: `src.data` is a live IDataObject.
    assert!(unsafe { copy_cf_hdrop_bytes(&src.data) }.is_none());
    assert_eq!(src.get_data_calls.load(Ordering::SeqCst), 1);
}
