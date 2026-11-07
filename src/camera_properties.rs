//! The camera driver's own settings window (exposure, focus, white balance, …), Windows only.
//!
//! This is the DirectShow property page every webcam driver provides, the same window OBS opens
//! with "Configure Video". Its changes apply to the device directly, so they also affect the
//! stream PiViz is capturing through Media Foundation.

use crate::webcam::PropertiesError;
use windows::Win32::Foundation::HWND;
use windows::Win32::Media::DirectShow::{IBaseFilter, ICreateDevEnum};
use windows::Win32::Media::MediaFoundation::{
    CLSID_SystemDeviceEnum, CLSID_VideoInputDeviceCategory,
};
use windows::Win32::System::Com::StructuredStorage::IPropertyBag;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoTaskMemFree, CoUninitialize, IMoniker,
};
use windows::Win32::System::Ole::{ISpecifyPropertyPages, OleCreatePropertyFrame};
use windows::Win32::System::Variant::{VARIANT, VT_BSTR, VariantClear};
use windows::core::{HSTRING, IUnknown, Interface, w};

impl From<windows::core::Error> for PropertiesError {
    fn from(e: windows::core::Error) -> Self {
        PropertiesError::Failed(e.message())
    }
}

/// Opens the settings window of the camera called `name` and returns when the user closes it.
/// The window is modal, so call this on its own thread.
pub fn show(name: &str) -> Result<(), PropertiesError> {
    // SAFETY: COM is initialized for this thread for the duration of the call, and every pointer
    // passed below comes from a live COM object or a local.
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        let result = show_initialized(name);
        CoUninitialize();
        result
    }
}

/// [`show`] once COM is initialized: finds the device, lists its settings pages and shows them.
unsafe fn show_initialized(name: &str) -> Result<(), PropertiesError> {
    let filter = unsafe { find_device(name)? }.ok_or(PropertiesError::NotFound)?;
    let pages: ISpecifyPropertyPages = filter.cast().map_err(|_| PropertiesError::NoSettings)?;
    let list = unsafe { pages.GetPages()? };
    if list.cElems == 0 {
        unsafe { CoTaskMemFree(Some(list.pElems.cast())) };
        return Err(PropertiesError::NoSettings);
    }
    let object: Option<IUnknown> = Some(filter.cast()?);
    let result = unsafe {
        OleCreatePropertyFrame(
            HWND::default(),
            0,
            0,
            &HSTRING::from(name),
            1,
            &object,
            list.cElems,
            list.pElems,
            0,
            None,
            None,
        )
    };
    unsafe { CoTaskMemFree(Some(list.pElems.cast())) };
    Ok(result?)
}

/// The DirectShow filter of the video device with this name (exact match first, then a device
/// whose name contains it).
unsafe fn find_device(name: &str) -> Result<Option<IBaseFilter>, PropertiesError> {
    let devices: ICreateDevEnum =
        unsafe { CoCreateInstance(&CLSID_SystemDeviceEnum, None, CLSCTX_INPROC_SERVER)? };
    let mut monikers = None;
    unsafe { devices.CreateClassEnumerator(&CLSID_VideoInputDeviceCategory, &mut monikers, 0)? };
    // No video device at all: the enumerator is not created.
    let Some(monikers) = monikers else {
        return Ok(None);
    };
    let mut partial = None;
    loop {
        let mut slot: [Option<IMoniker>; 1] = [None];
        if unsafe { monikers.Next(&mut slot, None) }.is_err() {
            break;
        }
        let Some(moniker) = slot[0].take() else {
            break;
        };
        let Some(friendly) = (unsafe { friendly_name(&moniker) }) else {
            continue;
        };
        if friendly == name {
            return Ok(Some(unsafe { moniker.BindToObject(None, None)? }));
        }
        if partial.is_none() && (friendly.contains(name) || name.contains(&friendly)) {
            partial = Some(moniker);
        }
    }
    match partial {
        Some(moniker) => Ok(Some(unsafe { moniker.BindToObject(None, None)? })),
        None => Ok(None),
    }
}

/// The `FriendlyName` of a DirectShow device, from its property bag.
unsafe fn friendly_name(moniker: &IMoniker) -> Option<String> {
    let bag: IPropertyBag = unsafe { moniker.BindToStorage(None, None) }.ok()?;
    let mut value = VARIANT::default();
    unsafe { bag.Read(w!("FriendlyName"), &mut value, None) }.ok()?;
    // SAFETY: the union is read through the field matching its type tag.
    let name = unsafe {
        let inner = &value.Anonymous.Anonymous;
        (inner.vt == VT_BSTR).then(|| inner.Anonymous.bstrVal.to_string())
    };
    let _ = unsafe { VariantClear(&mut value) };
    name
}
