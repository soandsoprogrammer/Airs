use anyhow::Result;
use async_trait::async_trait;
use publisher_core::*;
use tokio_util::sync::CancellationToken;
pub struct Simulation;
#[async_trait]
impl PlatformAdapter for Simulation {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            image_posts: false,
            name: "模拟测试",
            verified: false,
            separate_prepare: true,
            custom_cover: false,
            submit_enabled: true,
        }
    }
    async fn login(&self, _: &Store, _: &Account, _: CancellationToken) -> Result<SessionStatus> {
        Ok(SessionStatus::Ready)
    }
    async fn prepare(&self, ctx: RunContext) -> Result<Box<dyn Prepared>> {
        ctx.stage(JobState::Uploading, "模拟上传（没有连接真实平台）")?;
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        Ok(Box::new(SimulationPrepared))
    }
}
struct SimulationPrepared;
#[async_trait]
impl Prepared for SimulationPrepared {
    async fn submit(&mut self) -> Result<()> {
        Ok(())
    }
    async fn inspect_result(&mut self) -> Result<Option<Receipt>> {
        Ok(Some(Receipt {
            platform_id: "simulation-only".into(),
            evidence: "模拟适配器测试结果；未发布真实内容".into(),
            url: None,
        }))
    }
    async fn close(&mut self) -> Result<()> {
        Ok(())
    }
}
