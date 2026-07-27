use std::sync::mpsc::Sender;

use windows::{
    Win32::{
        Foundation::{
            DISP_E_UNKNOWNNAME, E_FAIL, E_NOTIMPL, MK_E_UNAVAILABLE, REGDB_E_CLASSNOTREG,
        },
        System::{
            Com::{
                CLSIDFromProgID, DISPATCH_FLAGS, DISPATCH_METHOD, DISPATCH_PROPERTYGET, DISPPARAMS,
                IConnectionPoint, IConnectionPointContainer, IDispatch, IDispatch_Impl, ITypeInfo,
            },
            Ole::GetActiveObject,
            Variant::VARIANT,
        },
    },
    core::{BSTR, Error, GUID, HSTRING, IUnknown, Interface, PCWSTR, implement},
};

use super::{PresentationApplication, SlidePage, SlideShowControlAction, SlideShowKey};
use crate::ink::PageKey;

pub(crate) const POWERPOINT_EVENT_IID: GUID =
    GUID::from_u128(0x914934c2_5a91_11cf_8700_00aa0060263b);

/// 一个可供 COM detector 尝试连接的 late-bound 应用候选。
#[derive(Debug, Clone, Copy)]
pub(crate) struct ComCandidate {
    pub application: PresentationApplication,
    pub prog_id: &'static str,
}

/// 当前已通过 COM 可靠读取到的活动放映快照。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActiveSlideShowSnapshot {
    pub key: SlideShowKey,
    pub page: SlidePage,
}

/// 连接活动 COM 对象时可用于诊断的失败类别。
#[derive(Debug)]
pub(crate) enum ActiveObjectError {
    ClassNotRegistered,
    NotRunning,
    Other(String),
}

/// 持有 connection point、事件 sink 和取消订阅 cookie。
pub(crate) struct EventSubscription {
    point: IConnectionPoint,
    _sink: IDispatch,
    cookie: u32,
}

impl Drop for EventSubscription {
    /// 在 detector STA 线程退出或重连前取消 COM 事件订阅。
    fn drop(&mut self) {
        // SAFETY: cookie 由同一个 connection point 的 Advise 返回，且仍在 STA 线程释放。
        if let Err(error) = unsafe { self.point.Unadvise(self.cookie) } {
            tracing::debug!(%error, "取消 PowerPoint/WPS COM 事件订阅失败");
        }
    }
}

#[implement(IDispatch)]
struct DispatchEventSink {
    event_sender: Sender<i32>,
}

#[allow(non_snake_case)]
impl IDispatch_Impl for DispatchEventSink_Impl {
    /// 事件 sink 不提供类型信息。
    fn GetTypeInfoCount(&self) -> windows::core::Result<u32> {
        Ok(0)
    }

    /// 事件 sink 不提供类型信息对象。
    fn GetTypeInfo(&self, _itinfo: u32, _lcid: u32) -> windows::core::Result<ITypeInfo> {
        Err(Error::from_hresult(E_NOTIMPL))
    }

    /// 事件 sink 不支持由名称反查成员标识。
    fn GetIDsOfNames(
        &self,
        _riid: *const GUID,
        _rgsznames: *const PCWSTR,
        _cnames: u32,
        _lcid: u32,
        _rgdispid: *mut i32,
    ) -> windows::core::Result<()> {
        Err(Error::from_hresult(DISP_E_UNKNOWNNAME))
    }

    /// 接收任意 PowerPoint 应用事件，并把原始 DISPID 交回 detector 做可靠状态差分。
    fn Invoke(
        &self,
        dispidmember: i32,
        _riid: *const GUID,
        _lcid: u32,
        _wflags: DISPATCH_FLAGS,
        _pdispparams: *const DISPPARAMS,
        _pvarresult: *mut VARIANT,
        _pexcepinfo: *mut windows::Win32::System::Com::EXCEPINFO,
        _puargerr: *mut u32,
    ) -> windows::core::Result<()> {
        let _ = self.event_sender.send(dispidmember);
        Ok(())
    }
}

/// 通过 ProgID 连接已运行的 PowerPoint/WPS COM 对象，不创建新应用实例。
pub(crate) fn connect_active_object(prog_id: &str) -> Result<IDispatch, ActiveObjectError> {
    let prog_id = HSTRING::from(prog_id);
    // SAFETY: CLSIDFromProgID 只读取有效、以 nul 结尾的 HSTRING。
    let clsid = unsafe { CLSIDFromProgID(&prog_id) }.map_err(classify_active_object_error)?;
    let mut unknown: Option<IUnknown> = None;
    // SAFETY: 输出指针指向有效 Option<IUnknown>，reserved 参数按 API 要求为空。
    unsafe { GetActiveObject(&clsid, None, &mut unknown) }.map_err(classify_active_object_error)?;
    let unknown = unknown.ok_or_else(|| {
        ActiveObjectError::Other("GetActiveObject 成功但未返回 IUnknown".to_owned())
    })?;
    unknown
        .cast::<IDispatch>()
        .map_err(|error| ActiveObjectError::Other(error.to_string()))
}

/// 通过稳定的 IUnknown 身份判断两个调度接口是否属于同一个 COM 对象。
pub(crate) fn same_com_identity(
    left: &IDispatch,
    right: &IDispatch,
) -> windows::core::Result<bool> {
    let left = left.cast::<IUnknown>()?;
    let right = right.cast::<IUnknown>()?;
    Ok(left.as_raw() == right.as_raw())
}

/// 读取 PowerPoint 兼容 Application.Visible，判断用户界面是否仍在运行。
pub(crate) fn application_is_visible(application: &IDispatch) -> windows::core::Result<bool> {
    let value = get_property(application, "Visible")?;
    if let Ok(visible) = bool::try_from(&value) {
        return Ok(visible);
    }
    Ok(i32::try_from(&value)? != 0)
}

/// 为 PowerPoint 兼容的 EApplication connection point 安装事件 sink。
pub(crate) fn subscribe_application_events(
    application: &IDispatch,
    event_sender: Sender<i32>,
) -> windows::core::Result<EventSubscription> {
    let container = application.cast::<IConnectionPointContainer>()?;
    // SAFETY: 使用 PowerPoint EApplication dispinterface 的稳定 IID 查询连接点。
    let point = unsafe { container.FindConnectionPoint(&POWERPOINT_EVENT_IID) }?;
    let sink: IDispatch = DispatchEventSink { event_sender }.into();
    // SAFETY: sink 在 EventSubscription 生命周期内保持存活，并在 Drop 中使用相同连接点取消。
    let cookie = unsafe { point.Advise(&sink) }?;
    Ok(EventSubscription {
        point,
        _sink: sink,
        cookie,
    })
}

/// 从活动应用读取当前放映窗口；未放映时可靠返回 `Ok(None)`。
pub(crate) fn query_active_slideshow(
    application: &IDispatch,
    fallback_application: PresentationApplication,
) -> windows::core::Result<Option<ActiveSlideShowSnapshot>> {
    let slideshow_windows = get_dispatch_property(application, "SlideShowWindows")?;
    let window_count = get_i32_property(&slideshow_windows, "Count")?;
    if window_count < 1 {
        return Ok(None);
    }

    let slideshow_window = invoke_dispatch(
        &slideshow_windows,
        "Item",
        DISPATCH_METHOD | DISPATCH_PROPERTYGET,
        vec![VARIANT::from(1_i32)],
    )?;
    let view = get_dispatch_property(&slideshow_window, "View")?;
    let current_position = get_i32_property(&view, "CurrentShowPosition")?;
    let Ok(current_position) = u32::try_from(current_position) else {
        return Ok(None);
    };
    let Some(page_key) = PageKey::new(current_position) else {
        return Ok(None);
    };

    let slide = get_dispatch_property(&view, "Slide").ok();
    let stable_slide_id = slide
        .as_ref()
        .and_then(|slide| get_i32_property(slide, "SlideID").ok())
        .map(i64::from);
    let window_id = get_i32_property(&slideshow_window, "HWND")
        .map(com_long_to_window_id)
        .unwrap_or_default();
    let presentation = get_dispatch_property(&slideshow_window, "Presentation").ok();
    let total_pages = presentation.as_ref().and_then(query_total_pages);
    let presentation_id = presentation
        .as_ref()
        .and_then(|presentation| get_string_property(presentation, "FullName").ok())
        .or_else(|| {
            presentation
                .as_ref()
                .and_then(|presentation| get_string_property(presentation, "Name").ok())
        })
        .unwrap_or_else(|| format!("window:{window_id}"));
    let application_name = get_string_property(application, "Name").ok();
    let application_kind =
        classify_application_kind(application_name.as_deref(), fallback_application);

    Ok(Some(ActiveSlideShowSnapshot {
        key: SlideShowKey::new(application_kind, presentation_id, window_id),
        page: SlidePage::new(page_key, stable_slide_id, total_pages),
    }))
}

/// 通过当前活动 SlideShowView 执行上一页、下一页或退出放映。
pub(crate) fn control_active_slideshow(
    application: &IDispatch,
    action: SlideShowControlAction,
) -> windows::core::Result<()> {
    let slideshow_windows = get_dispatch_property(application, "SlideShowWindows")?;
    if get_i32_property(&slideshow_windows, "Count")? < 1 {
        return Err(Error::new(E_FAIL, "当前没有活动放映窗口"));
    }
    let slideshow_window = invoke_dispatch(
        &slideshow_windows,
        "Item",
        DISPATCH_METHOD | DISPATCH_PROPERTYGET,
        vec![VARIANT::from(1_i32)],
    )?;
    let view = get_dispatch_property(&slideshow_window, "View")?;
    let method = match action {
        SlideShowControlAction::Previous => "Previous",
        SlideShowControlAction::Next => "Next",
        SlideShowControlAction::Exit => "Exit",
    };
    let _ = invoke(&view, method, DISPATCH_METHOD, Vec::new())?;
    Ok(())
}

/// 只对“全部幻灯片”和明确起止范围返回可靠的实际放映总数。
fn query_total_pages(presentation: &IDispatch) -> Option<u32> {
    const SHOW_ALL: i32 = 1;
    const SHOW_SLIDE_RANGE: i32 = 2;

    let settings = get_dispatch_property(presentation, "SlideShowSettings").ok()?;
    let slides = get_dispatch_property(presentation, "Slides").ok()?;
    let slide_count = get_i32_property(&slides, "Count").ok()?;
    match get_i32_property(&settings, "RangeType").ok()? {
        SHOW_ALL => count_visible_slides(&slides, 1, slide_count),
        SHOW_SLIDE_RANGE => {
            let first = get_i32_property(&settings, "StartingSlide").ok()?;
            let last = get_i32_property(&settings, "EndingSlide").ok()?;
            if first < 1 || last < first || last > slide_count {
                return None;
            }
            count_visible_slides(&slides, first, last)
        }
        _ => None,
    }
}

/// 统计放映范围内未隐藏的幻灯片数量；任一 COM 属性失败即视为不可靠。
fn count_visible_slides(slides: &IDispatch, first: i32, last: i32) -> Option<u32> {
    let mut visible_count = 0_u32;
    for index in first..=last {
        let slide = invoke_dispatch(
            slides,
            "Item",
            DISPATCH_METHOD | DISPATCH_PROPERTYGET,
            vec![VARIANT::from(index)],
        )
        .ok()?;
        let transition = get_dispatch_property(&slide, "SlideShowTransition").ok()?;
        let hidden = get_i32_property(&transition, "Hidden").ok()?;
        if hidden == 0 {
            visible_count = visible_count.checked_add(1)?;
        }
    }
    (visible_count > 0).then_some(visible_count)
}

/// 根据 HRESULT 把未安装、未运行和其他 COM 故障分开诊断。
fn classify_active_object_error(error: Error) -> ActiveObjectError {
    if error.code() == REGDB_E_CLASSNOTREG {
        ActiveObjectError::ClassNotRegistered
    } else if error.code() == MK_E_UNAVAILABLE {
        ActiveObjectError::NotRunning
    } else {
        ActiveObjectError::Other(error.to_string())
    }
}

/// 根据应用 Name 属性识别 WPS 兼容对象，属性不可读时使用候选类型。
fn classify_application_kind(
    application_name: Option<&str>,
    fallback: PresentationApplication,
) -> PresentationApplication {
    let name = application_name.unwrap_or_default().to_ascii_lowercase();
    if name.contains("wps") || name.contains("kingsoft") {
        PresentationApplication::Wps
    } else {
        fallback
    }
}

/// 保留 Office COM `Long` 中 HWND 的 32 位位模式，避免负值被符号扩展。
fn com_long_to_window_id(value: i32) -> i64 {
    i64::from(u32::from_ne_bytes(value.to_ne_bytes()))
}

/// 读取一个不带参数的 COM 属性。
fn get_property(dispatch: &IDispatch, name: &str) -> windows::core::Result<VARIANT> {
    invoke(dispatch, name, DISPATCH_PROPERTYGET, Vec::new())
}

/// 读取一个返回 IDispatch 的 COM 属性。
fn get_dispatch_property(dispatch: &IDispatch, name: &str) -> windows::core::Result<IDispatch> {
    let value = get_property(dispatch, name)?;
    IDispatch::try_from(&value)
}

/// 读取一个可转换为 i32 的 COM 属性。
fn get_i32_property(dispatch: &IDispatch, name: &str) -> windows::core::Result<i32> {
    let value = get_property(dispatch, name)?;
    i32::try_from(&value)
}

/// 读取一个 BSTR COM 属性并转换为 Rust 字符串。
fn get_string_property(dispatch: &IDispatch, name: &str) -> windows::core::Result<String> {
    let value = get_property(dispatch, name)?;
    let value = BSTR::try_from(&value)?;
    Ok(String::from_utf16_lossy(&value))
}

/// 调用一个返回 IDispatch 的 COM 方法或带参数属性。
fn invoke_dispatch(
    dispatch: &IDispatch,
    name: &str,
    flags: DISPATCH_FLAGS,
    arguments: Vec<VARIANT>,
) -> windows::core::Result<IDispatch> {
    let value = invoke(dispatch, name, flags, arguments)?;
    IDispatch::try_from(&value)
}

/// 解析成员名并通过 IDispatch::Invoke 执行 late-bound 调用。
fn invoke(
    dispatch: &IDispatch,
    name: &str,
    flags: DISPATCH_FLAGS,
    mut arguments: Vec<VARIANT>,
) -> windows::core::Result<VARIANT> {
    let member_name = HSTRING::from(name);
    let member_name = PCWSTR(member_name.as_ptr());
    let null_iid = GUID::zeroed();
    let mut dispid = 0_i32;
    // SAFETY: 名称指针、DISPID 输出和 IDispatch 在调用期间均保持有效。
    unsafe {
        dispatch.GetIDsOfNames(&null_iid, &member_name, 1, 0, &mut dispid)?;
    }

    arguments.reverse();
    let parameters = DISPPARAMS {
        rgvarg: if arguments.is_empty() {
            std::ptr::null_mut()
        } else {
            arguments.as_mut_ptr()
        },
        rgdispidNamedArgs: std::ptr::null_mut(),
        cArgs: u32::try_from(arguments.len()).unwrap_or(u32::MAX),
        cNamedArgs: 0,
    };
    let mut result = VARIANT::default();
    // SAFETY: DISPPARAMS 引用的参数和结果 VARIANT 在 Invoke 返回前保持有效。
    unsafe {
        dispatch.Invoke(
            dispid,
            &null_iid,
            0,
            flags,
            &parameters,
            Some(&mut result),
            None,
            None,
        )?;
    }
    Ok(result)
}
