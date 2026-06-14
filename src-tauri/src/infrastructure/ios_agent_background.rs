#![cfg(target_os = "ios")]
#![allow(unsafe_op_in_unsafe_fn)]

use std::collections::HashMap;
use std::ffi::{CStr, c_char};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use block2::RcBlock;
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, Sel};
use objc2_foundation::NSString;
use tauri::AppHandle;
use tokio::sync::oneshot;

const CONTINUED_PROCESSING_IDENTIFIER_PREFIX: &str =
    "com.tauritavern.client.agent.continued.";
const CONTINUED_PROCESSING_IDENTIFIER_PATTERN: &str =
    "com.tauritavern.client.agent.continued.*";
const BACKGROUND_TASK_INVALID: isize = -1;

type ExpirationHandler = Arc<dyn Fn(IosAgentBackgroundExpiration) + Send + Sync>;

#[derive(Debug, Clone)]
pub struct IosAgentBackgroundExpiration {
    pub mode: String,
    pub identifier: Option<String>,
}

#[derive(Debug, Clone)]
pub struct IosAgentBackgroundStartReport {
    pub mode: String,
    pub identifier: Option<String>,
    pub system_version: String,
    pub continued_processing_available: bool,
    pub fallback_reason: Option<String>,
}

pub struct IosAgentBackgroundActivity {
    app_handle: AppHandle,
    mode: String,
    continued_identifier: Option<String>,
    finite_background_task: Option<isize>,
    finished: Arc<AtomicBool>,
}

impl IosAgentBackgroundActivity {
    pub async fn begin_agent_run(
        app_handle: AppHandle,
        run_id: &str,
        title: String,
        subtitle: String,
        on_expiration: ExpirationHandler,
    ) -> Result<(Self, IosAgentBackgroundStartReport), String> {
        let identifier = continued_processing_identifier(run_id);
        let (sender, receiver) = oneshot::channel();
        let app_handle_for_main = app_handle.clone();
        let title_for_main = title;
        let subtitle_for_main = subtitle;

        app_handle
            .run_on_main_thread(move || {
                let result = unsafe {
                    begin_agent_run_on_main_thread(
                        app_handle_for_main,
                        identifier,
                        title_for_main,
                        subtitle_for_main,
                        on_expiration,
                    )
                };
                let _ = sender.send(result);
            })
            .map_err(|error| error.to_string())?;

        receiver
            .await
            .map_err(|_| "iOS background activity setup channel closed".to_string())?
    }

    pub fn report_progress(&self, completed: i64, total: i64) {
        if self.finished.load(Ordering::SeqCst) {
            return;
        }

        let Some(identifier) = self.continued_identifier.clone() else {
            return;
        };
        let app_handle = self.app_handle.clone();
        let _ = app_handle.run_on_main_thread(move || unsafe {
            update_continued_processing_progress(identifier.as_str(), completed, total);
        });
    }

    pub fn finish(&self, success: bool) {
        if self.finished.swap(true, Ordering::SeqCst) {
            return;
        }

        let mode = self.mode.clone();
        let continued_identifier = self.continued_identifier.clone();
        let finite_background_task = self.finite_background_task;
        let app_handle = self.app_handle.clone();

        let _ = app_handle.run_on_main_thread(move || unsafe {
            if let Some(identifier) = continued_identifier {
                complete_continued_processing_task(identifier.as_str(), success);
                remove_expiration_handler(identifier.as_str());
            }

            if let Some(task_id) = finite_background_task {
                end_finite_background_task(task_id);
            }

            tracing::debug!(
                "Finished iOS Agent background activity mode={} success={}",
                mode,
                success
            );
        });
    }
}

pub fn prepare_agent_background_handlers() -> Result<(), String> {
    unsafe { prepare_agent_background_handlers_on_main_thread() }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct NSOperatingSystemVersion {
    major_version: isize,
    minor_version: isize,
    patch_version: isize,
}

unsafe extern "C" {
    fn sel_registerName(name: *const c_char) -> Sel;

    #[link_name = "objc_msgSend"]
    fn objc_msgSend_operating_system_version(
        receiver: *mut AnyObject,
        selector: Sel,
    ) -> NSOperatingSystemVersion;
}

fn cstr(bytes: &'static [u8]) -> &'static CStr {
    unsafe { CStr::from_bytes_with_nul_unchecked(bytes) }
}

fn selector(bytes: &'static [u8]) -> Sel {
    unsafe { sel_registerName(cstr(bytes).as_ptr()) }
}

fn objc_class(name: &'static [u8]) -> Option<&'static AnyClass> {
    AnyClass::get(cstr(name))
}

fn object_responds_to_selector(object: &AnyObject, name: &'static [u8]) -> bool {
    let selector = selector(name);
    let responds: Bool = unsafe { msg_send![object, respondsToSelector: selector] };
    responds.as_bool()
}

fn class_instances_respond_to_selector(class: &AnyClass, name: &'static [u8]) -> bool {
    let selector = selector(name);
    let responds: Bool = unsafe { msg_send![class, instancesRespondToSelector: selector] };
    responds.as_bool()
}

unsafe fn begin_agent_run_on_main_thread(
    app_handle: AppHandle,
    identifier: String,
    title: String,
    subtitle: String,
    on_expiration: ExpirationHandler,
) -> Result<(IosAgentBackgroundActivity, IosAgentBackgroundStartReport), String> {
    let system_version = system_version_string();

    match try_begin_continued_processing(
        app_handle.clone(),
        identifier.clone(),
        title.clone(),
        subtitle.clone(),
        on_expiration.clone(),
        system_version.clone(),
    ) {
        Ok(result) => return Ok(result),
        Err(continued_error) => {
            tracing::info!(
                "Falling back to finite iOS background task for Agent run: {}",
                continued_error
            );
            begin_finite_background_task(
                app_handle,
                title,
                on_expiration,
                system_version,
                Some(continued_error),
            )
        }
    }
}

unsafe fn prepare_agent_background_handlers_on_main_thread() -> Result<(), String> {
    let version = operating_system_version()
        .ok_or_else(|| "NSProcessInfo operatingSystemVersion is unavailable".to_string())?;
    if version.major_version < 26 {
        return Ok(());
    }

    let scheduler_class = objc_class(b"BGTaskScheduler\0")
        .ok_or_else(|| "BGTaskScheduler class is unavailable".to_string())?;
    let scheduler: Retained<AnyObject> = msg_send![scheduler_class, sharedScheduler];
    ensure_continued_processing_registered(&scheduler)
}

unsafe fn try_begin_continued_processing(
    app_handle: AppHandle,
    identifier: String,
    title: String,
    subtitle: String,
    on_expiration: ExpirationHandler,
    system_version: String,
) -> Result<(IosAgentBackgroundActivity, IosAgentBackgroundStartReport), String> {
    let version = operating_system_version()
        .ok_or_else(|| "NSProcessInfo operatingSystemVersion is unavailable".to_string())?;
    if version.major_version < 26 {
        return Err(format!(
            "iOS {}.{}.{} does not support BGContinuedProcessingTask",
            version.major_version, version.minor_version, version.patch_version
        ));
    }

    let request_class =
        objc_class(b"BGContinuedProcessingTaskRequest\0").ok_or_else(|| {
            "BGContinuedProcessingTaskRequest class is unavailable".to_string()
        })?;
    let scheduler_class = objc_class(b"BGTaskScheduler\0")
        .ok_or_else(|| "BGTaskScheduler class is unavailable".to_string())?;
    let scheduler: Retained<AnyObject> = msg_send![scheduler_class, sharedScheduler];

    ensure_continued_processing_registered(&scheduler)?;
    insert_expiration_handler(identifier.as_str(), on_expiration);

    if !object_responds_to_selector(&scheduler, b"submitTaskRequest:error:\0") {
        remove_expiration_handler(identifier.as_str());
        return Err("BGTaskScheduler submitTaskRequest:error: is unavailable".to_string());
    }

    let request = create_continued_processing_request(request_class, &identifier, &title, &subtitle)
        .map_err(|message| {
            remove_expiration_handler(identifier.as_str());
            message
        })?;
    let Some(request_ref) = request.as_ref() else {
        release_object(request);
        remove_expiration_handler(identifier.as_str());
        return Err("BGContinuedProcessingTaskRequest initializer returned null".to_string());
    }

    let mut error: *mut AnyObject = std::ptr::null_mut();
    let submitted: Bool = msg_send![
        &*scheduler,
        submitTaskRequest: request_ref
        error: &mut error
    ];
    release_object(request);
    if !submitted.as_bool() {
        remove_expiration_handler(identifier.as_str());
        return Err(format!(
            "BGContinuedProcessingTaskRequest submission failed: {}",
            error_description(error).unwrap_or_else(|| "unknown error".to_string())
        ));
    }

    let activity = IosAgentBackgroundActivity {
        app_handle,
        mode: "continued_processing".to_string(),
        continued_identifier: Some(identifier.clone()),
        finite_background_task: None,
        finished: Arc::new(AtomicBool::new(false)),
    };
    let report = IosAgentBackgroundStartReport {
        mode: "continued_processing".to_string(),
        identifier: Some(identifier),
        system_version,
        continued_processing_available: true,
        fallback_reason: None,
    };

    Ok((activity, report))
}

unsafe fn begin_finite_background_task(
    app_handle: AppHandle,
    title: String,
    on_expiration: ExpirationHandler,
    system_version: String,
    fallback_reason: Option<String>,
) -> Result<(IosAgentBackgroundActivity, IosAgentBackgroundStartReport), String> {
    let app = shared_application()?;
    let title = NSString::from_str(title.as_str());
    let expiration = IosAgentBackgroundExpiration {
        mode: "finite_background_task".to_string(),
        identifier: None,
    };
    let expiration_block: RcBlock<dyn Fn()> = RcBlock::new(move || {
        on_expiration(expiration.clone());
    });
    let task_id: isize = msg_send![
        &*app,
        beginBackgroundTaskWithName: &*title
        expirationHandler: RcBlock::as_ptr(&expiration_block)
    ];

    if task_id == BACKGROUND_TASK_INVALID {
        return Err("UIApplication.beginBackgroundTask returned invalid identifier".to_string());
    }
    std::mem::forget(expiration_block);

    let activity = IosAgentBackgroundActivity {
        app_handle,
        mode: "finite_background_task".to_string(),
        continued_identifier: None,
        finite_background_task: Some(task_id),
        finished: Arc::new(AtomicBool::new(false)),
    };
    let report = IosAgentBackgroundStartReport {
        mode: "finite_background_task".to_string(),
        identifier: None,
        system_version,
        continued_processing_available: false,
        fallback_reason,
    };

    Ok((activity, report))
}

unsafe fn create_continued_processing_request(
    request_class: &AnyClass,
    identifier: &str,
    title: &str,
    subtitle: &str,
) -> Result<*mut AnyObject, String> {
    let identifier = NSString::from_str(identifier);
    let title = NSString::from_str(title);
    let subtitle = NSString::from_str(subtitle);

    if class_instances_respond_to_selector(
        request_class,
        b"initWithIdentifier:title:subtitle:\0",
    ) {
        let allocated: *mut AnyObject = msg_send![request_class, alloc];
        let Some(allocated) = allocated.as_ref() else {
            return Err("BGContinuedProcessingTaskRequest alloc returned null".to_string());
        };
        let request: *mut AnyObject = msg_send![
            allocated,
            initWithIdentifier: &*identifier
            title: &*title
            subtitle: &*subtitle
        ];
        if request.is_null() {
            return Err(
                "BGContinuedProcessingTaskRequest title initializer returned null".to_string(),
            );
        }
        return Ok(request);
    }

    if class_instances_respond_to_selector(request_class, b"initWithIdentifier:\0") {
        let allocated: *mut AnyObject = msg_send![request_class, alloc];
        let Some(allocated) = allocated.as_ref() else {
            return Err("BGContinuedProcessingTaskRequest alloc returned null".to_string());
        };
        let request: *mut AnyObject = msg_send![allocated, initWithIdentifier: &*identifier];
        let Some(request_ref) = request.as_ref() else {
            return Err(
                "BGContinuedProcessingTaskRequest identifier initializer returned null".to_string(),
            );
        };
        set_string_property_if_available(request_ref, b"setTitle:\0", &title);
        set_string_property_if_available(request_ref, b"setSubtitle:\0", &subtitle);
        return Ok(request);
    }

    Err("BGContinuedProcessingTaskRequest has no supported initializer".to_string())
}

unsafe fn set_string_property_if_available(
    object: &AnyObject,
    setter: &'static [u8],
    value: &NSString,
) {
    if object_responds_to_selector(object, setter) {
        match setter {
            b"setTitle:\0" => {
                let _: () = msg_send![object, setTitle: value];
            }
            b"setSubtitle:\0" => {
                let _: () = msg_send![object, setSubtitle: value];
            }
            _ => {}
        }
    }
}

static CONTINUED_PROCESSING_REGISTERED: AtomicBool = AtomicBool::new(false);

unsafe fn ensure_continued_processing_registered(scheduler: &AnyObject) -> Result<(), String> {
    if CONTINUED_PROCESSING_REGISTERED.load(Ordering::SeqCst) {
        return Ok(());
    }
    if !object_responds_to_selector(
        scheduler,
        b"registerForTaskWithIdentifier:usingQueue:launchHandler:\0",
    ) {
        return Err(
            "BGTaskScheduler registerForTaskWithIdentifier:usingQueue:launchHandler: is unavailable"
                .to_string(),
        );
    }

    let identifier = NSString::from_str(CONTINUED_PROCESSING_IDENTIFIER_PATTERN);
    let handler: RcBlock<dyn Fn(*mut AnyObject)> = RcBlock::new(move |task| unsafe {
        handle_continued_processing_task(task);
    });
    let queue: Option<&AnyObject> = None;
    let registered: Bool = msg_send![
        scheduler,
        registerForTaskWithIdentifier: &*identifier
        usingQueue: queue
        launchHandler: RcBlock::as_ptr(&handler)
    ];

    if registered.as_bool() {
        CONTINUED_PROCESSING_REGISTERED.store(true, Ordering::SeqCst);
        std::mem::forget(handler);
        Ok(())
    } else {
        Err(format!(
            "BGTaskScheduler registration failed for {}",
            CONTINUED_PROCESSING_IDENTIFIER_PATTERN
        ))
    }
}

unsafe fn handle_continued_processing_task(task: *mut AnyObject) {
    let Some(task) = task.as_ref() else {
        return;
    };
    let identifier = task_identifier(task).unwrap_or_default();
    if identifier.is_empty() {
        return;
    }
    if !has_expiration_handler(identifier.as_str()) {
        let _: () = msg_send![task, setTaskCompletedWithSuccess: false];
        return;
    }

    retain_continued_processing_task(identifier.as_str(), task);

    let expiration_identifier = identifier.clone();
    let expiration_block: RcBlock<dyn Fn()> = RcBlock::new(move || {
        invoke_expiration_handler(expiration_identifier.as_str(), "continued_processing");
    });
    let _: () = msg_send![
        task,
        setExpirationHandler: RcBlock::as_ptr(&expiration_block)
    ];
    std::mem::forget(expiration_block);

    update_continued_processing_progress(identifier.as_str(), 1, 100);
}

fn expiration_handlers() -> &'static Mutex<HashMap<String, ExpirationHandler>> {
    static HANDLERS: OnceLock<Mutex<HashMap<String, ExpirationHandler>>> = OnceLock::new();
    HANDLERS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn continued_processing_tasks() -> &'static Mutex<HashMap<String, usize>> {
    static TASKS: OnceLock<Mutex<HashMap<String, usize>>> = OnceLock::new();
    TASKS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn insert_expiration_handler(identifier: &str, handler: ExpirationHandler) {
    expiration_handlers()
        .lock()
        .expect("iOS background expiration handlers mutex poisoned")
        .insert(identifier.to_string(), handler);
}

fn remove_expiration_handler(identifier: &str) {
    expiration_handlers()
        .lock()
        .expect("iOS background expiration handlers mutex poisoned")
        .remove(identifier);
}

fn has_expiration_handler(identifier: &str) -> bool {
    expiration_handlers()
        .lock()
        .expect("iOS background expiration handlers mutex poisoned")
        .contains_key(identifier)
}

fn invoke_expiration_handler(identifier: &str, mode: &str) {
    let handler = expiration_handlers()
        .lock()
        .expect("iOS background expiration handlers mutex poisoned")
        .get(identifier)
        .cloned();
    if let Some(handler) = handler {
        handler(IosAgentBackgroundExpiration {
            mode: mode.to_string(),
            identifier: Some(identifier.to_string()),
        });
    }
}

unsafe fn retain_continued_processing_task(identifier: &str, task: &AnyObject) {
    let retained: *mut AnyObject = msg_send![task, retain];
    let previous = continued_processing_tasks()
        .lock()
        .expect("iOS continued processing task mutex poisoned")
        .insert(identifier.to_string(), retained as usize);
    if let Some(previous) = previous {
        release_object(previous as *mut AnyObject);
    }
}

unsafe fn complete_continued_processing_task(identifier: &str, success: bool) {
    let task = continued_processing_tasks()
        .lock()
        .expect("iOS continued processing task mutex poisoned")
        .remove(identifier);
    let Some(task) = task else {
        return;
    };

    let task = task as *mut AnyObject;
    if let Some(task_ref) = task.as_ref() {
        let _: () = msg_send![task_ref, setTaskCompletedWithSuccess: success];
    }
    release_object(task);
}

unsafe fn update_continued_processing_progress(identifier: &str, completed: i64, total: i64) {
    let task = continued_processing_tasks()
        .lock()
        .expect("iOS continued processing task mutex poisoned")
        .get(identifier)
        .copied();
    let Some(task) = task else {
        return;
    };

    let Some(task_ref) = (task as *mut AnyObject).as_ref() else {
        return;
    };
    if !object_responds_to_selector(task_ref, b"progress\0") {
        return;
    }
    let progress: Option<Retained<AnyObject>> = msg_send![task_ref, progress];
    let Some(progress) = progress else {
        return;
    };
    let total = total.max(1);
    let completed = completed.clamp(0, total);
    let _: () = msg_send![&*progress, setTotalUnitCount: total];
    let _: () = msg_send![&*progress, setCompletedUnitCount: completed];
}

unsafe fn end_finite_background_task(task_id: isize) {
    if task_id == BACKGROUND_TASK_INVALID {
        return;
    }
    if let Ok(app) = shared_application() {
        let _: () = msg_send![&*app, endBackgroundTask: task_id];
    }
}

unsafe fn shared_application() -> Result<Retained<AnyObject>, String> {
    let ui_application =
        objc_class(b"UIApplication\0")
            .ok_or_else(|| "UIApplication class is unavailable".to_string())?;
    let app: Option<Retained<AnyObject>> = msg_send![ui_application, sharedApplication];
    app.ok_or_else(|| "UIApplication.sharedApplication returned null".to_string())
}

unsafe fn operating_system_version() -> Option<NSOperatingSystemVersion> {
    let process_info_class = objc_class(b"NSProcessInfo\0")?;
    let process_info: Retained<AnyObject> = msg_send![process_info_class, processInfo];
    let process_info = (&*process_info as *const AnyObject).cast_mut();
    Some(objc_msgSend_operating_system_version(
        process_info,
        selector(b"operatingSystemVersion\0"),
    ))
}

unsafe fn system_version_string() -> String {
    operating_system_version()
        .map(|version| {
            format!(
                "{}.{}.{}",
                version.major_version, version.minor_version, version.patch_version
            )
        })
        .unwrap_or_else(|| "unknown".to_string())
}

unsafe fn task_identifier(task: &AnyObject) -> Option<String> {
    let identifier: Option<Retained<NSString>> = msg_send![task, identifier];
    identifier.map(|value| value.to_string())
}

unsafe fn error_description(error: *mut AnyObject) -> Option<String> {
    let error = error.as_ref()?;
    let description: Option<Retained<NSString>> = msg_send![error, localizedDescription];
    description.map(|value| value.to_string())
}

unsafe fn release_object(object: *mut AnyObject) {
    if let Some(object) = object.as_ref() {
        let _: () = msg_send![object, release];
    }
}

fn continued_processing_identifier(run_id: &str) -> String {
    let suffix = run_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string();

    format!(
        "{}{}",
        CONTINUED_PROCESSING_IDENTIFIER_PREFIX,
        if suffix.is_empty() {
            "run".to_string()
        } else {
            suffix
        }
    )
}
