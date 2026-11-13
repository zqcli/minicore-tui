//! Selections for a new Session and committed settings of the current Session.

use super::App;
use minicore_host_protocol::*;

impl App {
    pub fn model_id(&self) -> Option<&str> {
        self.resident
            .current()
            .map(|s| s.info.config.model.as_str())
            .or_else(|| self.launch.model.as_ref().map(ModelRef::as_str))
    }
    pub fn reasoning(&self) -> Option<ReasoningPreference> {
        self.resident
            .current()
            .map(|s| s.info.config.reasoning)
            .or(self.launch.reasoning)
    }
    pub fn reasoning_levels(&self) -> Vec<ReasoningPreference> {
        self.models
            .iter()
            .find(|model| Some(model.model.as_str()) == self.model_id())
            .map(|model| model.supported_reasoning.clone())
            .unwrap_or_default()
    }
    pub(super) fn set_model(&mut self, model: ModelRef) {
        if self.resident.current().is_some() {
            self.update_session(SessionConfigPatch {
                model: Some(model),
                ..Default::default()
            });
        } else {
            self.launch.model = Some(model);
        }
    }
    pub(super) fn set_reasoning(&mut self, reasoning: ReasoningPreference) {
        if self.resident.current().is_some() {
            self.update_session(SessionConfigPatch {
                reasoning: Some(reasoning),
                ..Default::default()
            });
        } else {
            self.launch.reasoning = Some(reasoning);
        }
    }
    pub(super) fn creation_overrides(&self) -> SessionOverrides {
        SessionOverrides(SessionConfigPatch {
            model: self.launch.model.clone(),
            reasoning: self.launch.reasoning,
            workspace: NullablePatch::Set(WorkspaceRef {
                path: self.workspace.to_string_lossy().into_owned(),
            }),
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::NativeHost;

    #[tokio::test(start_paused = true)]
    async fn an_unopened_agent_never_inherits_the_new_agent_configuration_for_display() {
        let fixture = NativeHost::new().await;
        let (tx, _) = tokio::sync::mpsc::channel(crate::app::tasks::MESSAGE_CAPACITY);
        let mut app = App::new(fixture.client.clone(), tx, std::path::PathBuf::from("."));
        app.selected_agent = Some(fixture.target.agent_id.clone());
        app.new_agent.session_defaults.model = "startup/not-selected".parse().unwrap();
        app.new_agent.session_defaults.reasoning = ReasoningPreference::High;
        assert_eq!(app.model_id(), None);
        assert_eq!(app.reasoning(), None);
        assert!(app.reasoning_levels().is_empty());
        app.launch.model = Some("fixture/explicit".parse().unwrap());
        app.launch.reasoning = Some(ReasoningPreference::Low);
        assert_eq!(app.model_id(), Some("fixture/explicit"));
        assert_eq!(app.reasoning(), Some(ReasoningPreference::Low));
        drop(app);
        fixture.finish().await;
    }

    #[tokio::test(start_paused = true)]
    async fn committed_session_settings_and_the_native_model_catalog_control_the_picker() {
        use minicore_host_core::ModelDescription;
        let fixture = NativeHost::new().await;
        let client = fixture.client.session(fixture.target.clone());
        let info = client.info(CallOptions::default()).await.unwrap();
        let state = client.state(CallOptions::default()).await.unwrap();
        let model = info.config.model.clone();
        let reasoning = info.config.reasoning;
        let (tx, _) = tokio::sync::mpsc::channel(crate::app::tasks::MESSAGE_CAPACITY);
        let mut app = App::new(fixture.client.clone(), tx, std::path::PathBuf::from("."));
        app.launch.model = Some("fixture/next-session".parse().unwrap());
        app.launch.reasoning = Some(ReasoningPreference::High);
        app.resident
            .insert(crate::app::Session::new(info, state, 1, client));
        app.resident.select(Some(1));
        assert_eq!(app.model_id(), Some(model.as_str()));
        assert_eq!(app.reasoning(), Some(reasoning));
        let description: ModelDescription = serde_json::from_value(serde_json::json!({
            "model":model,"api":"responses","context_window":1000,
            "supported_reasoning":["low","high"],"supports_tools":true
        }))
        .unwrap();
        app.models = vec![description];
        assert_eq!(
            app.reasoning_levels(),
            [ReasoningPreference::Low, ReasoningPreference::High]
        );
        drop(app);
        fixture.finish().await;
    }
}
