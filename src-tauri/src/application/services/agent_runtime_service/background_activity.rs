use std::sync::Arc;

use serde_json::json;
use tokio::sync::watch;

use super::AgentRuntimeService;
use crate::domain::models::agent::{AgentRun, AgentRunEventLevel, AgentRunStatus, WorkspacePath};

#[cfg(target_os = "ios")]
use crate::infrastructure::ios_agent_background::{
    IosAgentBackgroundActivity, IosAgentBackgroundExpiration, IosAgentBackgroundStartReport,
};

pub(super) struct AgentBackgroundActivity {
    #[cfg(target_os = "ios")]
    ios: IosAgentBackgroundActivity,
}

impl AgentRuntimeService {
    pub(super) async fn begin_background_activity_for_run(
        self: &Arc<Self>,
        run: &AgentRun,
        cancel_sender: watch::Sender<bool>,
    ) {
        let Some(activity) = AgentBackgroundActivity::begin(self.clone(), run, cancel_sender).await
        else {
            return;
        };

        self.background_activities
            .write()
            .await
            .insert(run.id.clone(), activity);
    }

    pub(super) async fn report_background_progress(
        &self,
        run_id: &str,
        status: AgentRunStatus,
    ) {
        let Some(progress) = background_progress_for_status(status) else {
            return;
        };
        if let Some(activity) = self.background_activities.read().await.get(run_id) {
            activity.report_progress(progress.completed, progress.total);
        }
    }

    pub(super) async fn finish_background_activity(&self, run_id: &str, success: bool) {
        let activity = self.background_activities.write().await.remove(run_id);
        if let Some(activity) = activity {
            activity.finish(success);
            let _ = self
                .event(
                    run_id,
                    AgentRunEventLevel::Info,
                    "ios_agent_background_activity_finished",
                    json!({ "success": success }),
                )
                .await;
        }
    }

    pub(super) async fn handle_background_activity_expiration(
        &self,
        run_id: String,
        mode: String,
        identifier: Option<String>,
    ) {
        let event = self
            .event(
                run_id.as_str(),
                AgentRunEventLevel::Warn,
                "ios_agent_background_activity_expired",
                json!({
                    "mode": mode,
                    "identifier": identifier,
                    "action": "checkpoint_and_cancel",
                }),
            )
            .await;

        if let Ok(event) = event {
            let paths: [WorkspacePath; 0] = [];
            if let Ok(checkpoint) = self
                .checkpoint_repository
                .create_checkpoint(
                    run_id.as_str(),
                    "ios_background_expiration",
                    event.seq,
                    &paths,
                )
                .await
            {
                let _ = self
                    .event(
                        run_id.as_str(),
                        AgentRunEventLevel::Info,
                        "checkpoint_created",
                        json!({
                            "checkpointId": checkpoint.id,
                            "reason": "ios_background_expiration",
                        }),
                    )
                    .await;
            }
        }

        let _ = self.cancel_unfinished_child_tasks(run_id.as_str()).await;
        self.clear_pending_host_requests_for_run(run_id.as_str()).await;
        if let Ok(run) = self.run_repository.load_run(run_id.as_str()).await {
            if !matches!(
                run.status,
                AgentRunStatus::Completed
                    | AgentRunStatus::PartialSuccess
                    | AgentRunStatus::Cancelled
                    | AgentRunStatus::Failed
            ) {
                let _ = self
                    .transition_status(run_id.as_str(), AgentRunStatus::Cancelling)
                    .await;
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct BackgroundProgress {
    completed: i64,
    total: i64,
}

fn background_progress_for_status(status: AgentRunStatus) -> Option<BackgroundProgress> {
    let completed = match status {
        AgentRunStatus::Created => 1,
        AgentRunStatus::InitializingWorkspace => 10,
        AgentRunStatus::AssemblingContext => 20,
        AgentRunStatus::CallingModel => 40,
        AgentRunStatus::DispatchingTool => 60,
        AgentRunStatus::ApplyingWorkspacePatch => 68,
        AgentRunStatus::CreatingCheckpoint => 76,
        AgentRunStatus::AwaitingHostCommit => 82,
        AgentRunStatus::Finishing => 92,
        AgentRunStatus::Completed | AgentRunStatus::PartialSuccess => 100,
        AgentRunStatus::Cancelling | AgentRunStatus::Cancelled | AgentRunStatus::Failed => 100,
    };

    Some(BackgroundProgress {
        completed,
        total: 100,
    })
}

impl AgentBackgroundActivity {
    async fn begin(
        service: Arc<AgentRuntimeService>,
        run: &AgentRun,
        cancel_sender: watch::Sender<bool>,
    ) -> Option<Self> {
        #[cfg(target_os = "ios")]
        {
            return Self::begin_ios(service, run, cancel_sender).await;
        }

        #[cfg(not(target_os = "ios"))]
        {
            let _ = (service, run, cancel_sender);
            None
        }
    }

    #[cfg(target_os = "ios")]
    async fn begin_ios(
        service: Arc<AgentRuntimeService>,
        run: &AgentRun,
        cancel_sender: watch::Sender<bool>,
    ) -> Option<Self> {
        let app_handle = service.app_handle.clone()?;
        let run_id = run.id.clone();
        let expiration_service = service.clone();
        let expiration_cancel = cancel_sender.clone();

        let on_expiration = Arc::new(move |expiration: IosAgentBackgroundExpiration| {
            let _ = expiration_cancel.send(true);
            let service = expiration_service.clone();
            let run_id = run_id.clone();
            tauri::async_runtime::spawn(async move {
                service
                    .handle_background_activity_expiration(
                        run_id,
                        expiration.mode,
                        expiration.identifier,
                    )
                    .await;
            });
        });

        let title = "TauriTavern Agent Run".to_string();
        let subtitle = format!("{} ({})", run.generation_type, short_run_id(run.id.as_str()));
        let start = IosAgentBackgroundActivity::begin_agent_run(
            app_handle,
            run.id.as_str(),
            title,
            subtitle,
            on_expiration,
        )
        .await;

        match start {
            Ok((ios, report)) => {
                emit_ios_start_event(&service, run.id.as_str(), &report).await;
                Some(Self { ios })
            }
            Err(message) => {
                let _ = service
                    .event(
                        run.id.as_str(),
                        AgentRunEventLevel::Warn,
                        "ios_agent_background_activity_unavailable",
                        json!({ "message": message }),
                    )
                    .await;
                None
            }
        }
    }

    fn report_progress(&self, completed: i64, total: i64) {
        #[cfg(target_os = "ios")]
        self.ios.report_progress(completed, total);

        #[cfg(not(target_os = "ios"))]
        let _ = (completed, total);
    }

    fn finish(&self, success: bool) {
        #[cfg(target_os = "ios")]
        self.ios.finish(success);

        #[cfg(not(target_os = "ios"))]
        let _ = success;
    }
}

#[cfg(target_os = "ios")]
async fn emit_ios_start_event(
    service: &AgentRuntimeService,
    run_id: &str,
    report: &IosAgentBackgroundStartReport,
) {
    let _ = service
        .event(
            run_id,
            AgentRunEventLevel::Info,
            "ios_agent_background_activity_started",
            json!({
                "mode": report.mode.as_str(),
                "identifier": report.identifier.as_deref(),
                "systemVersion": report.system_version.as_str(),
                "continuedProcessingAvailable": report.continued_processing_available,
                "fallbackReason": report.fallback_reason.as_deref(),
            }),
        )
        .await;
}

fn short_run_id(run_id: &str) -> &str {
    run_id.rsplit_once('_').map(|(_, tail)| tail).unwrap_or(run_id)
}
