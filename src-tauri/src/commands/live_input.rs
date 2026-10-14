//! get_live_input_snapshot —— 实时输入快照 Tauri 命令（DEVPLAN §4.4）。
//!
//! 克隆 `Arc<LiveInputClient>` 进 `spawn_blocking`；正常 IPC 错误返回结构化
//! [`LiveSnapshotReply`]，仅 join 失败才 `Err(String)`。

use std::sync::Arc;

use clrecoder_core::live_input::LiveSnapshotReply;
use tauri::State;

use crate::live_input::LiveInputClient;

/// 异步拉取一帧实时快照（§4.4）。
#[tauri::command]
pub async fn get_live_input_snapshot(
    client: State<'_, Arc<LiveInputClient>>,
) -> Result<LiveSnapshotReply, String> {
    let client = client.inner().clone();
    tauri::async_runtime::spawn_blocking(move || client.snapshot())
        .await
        .map_err(|e| format!("live input 后台任务失败: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clrecoder_core::live_input::{LiveErrorCode, LIVE_PIPE_NAME};

    #[test]
    fn live_input_shared_client_snapshot_is_structured() {
        let client = Arc::new(LiveInputClient::new());
        assert_eq!(client.pipe_name_for_test(), LIVE_PIPE_NAME);
        let reply = client.snapshot();
        // 生产端点在测试环境通常不存在 → 结构化 not_running（不是 Err/panic）。
        assert_eq!(
            reply.error.as_ref().map(|e| e.code),
            Some(LiveErrorCode::NotRunning),
            "{reply:?}"
        );
        assert!(reply.snapshot.is_none());
    }
}
