use std::time::Instant;

use zinc_proto::AgentInfo;

pub struct App {
    pub agents: Vec<AgentInfo>,
    pub selected: usize,
    /// Transient status message (errors, confirmations) with expiry time.
    pub status: Option<(String, Instant)>,
}

impl App {
    pub fn new() -> Self {
        Self {
            agents: Vec::new(),
            selected: 0,
            status: None,
        }
    }

    /// Replace the agent list, preserving selection as best we can.
    pub fn set_agents(&mut self, agents: Vec<AgentInfo>) {
        // Try to keep the same agent selected by ID
        let prev_id = self.selected_agent().map(|a| a.id.clone());
        self.agents = agents;
        self.sort_agents();
        if let Some(id) = prev_id {
            if let Some(pos) = self.agents.iter().position(|a| a.id == id) {
                self.selected = pos;
                return;
            }
        }
        self.clamp_selection();
    }

    /// Sort: blocked/input first (need attention), then by ID.
    fn sort_agents(&mut self) {
        use zinc_proto::AgentState;
        self.agents.sort_by(|a, b| {
            let priority = |s: &AgentState| match s {
                AgentState::Blocked => 0,
                AgentState::Input => 1,
                AgentState::Working => 2,
                AgentState::Idle => 3,
            };
            priority(&a.state)
                .cmp(&priority(&b.state))
                .then(a.id.cmp(&b.id))
        });
    }

    pub fn select_next(&mut self) {
        if !self.agents.is_empty() {
            self.selected = (self.selected + 1).min(self.agents.len() - 1);
        }
    }

    pub fn select_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn selected_agent(&self) -> Option<&AgentInfo> {
        self.agents.get(self.selected)
    }

    fn clamp_selection(&mut self) {
        if self.agents.is_empty() {
            self.selected = 0;
        } else {
            self.selected = self.selected.min(self.agents.len() - 1);
        }
    }

    /// Add a new agent to the list.
    pub fn add_agent(&mut self, info: AgentInfo) {
        // Avoid duplicates
        if !self.agents.iter().any(|a| a.id == info.id) {
            self.agents.push(info);
            self.sort_agents();
        }
    }

    /// Update a single agent's state. Returns true if the agent was found.
    pub fn update_state(&mut self, id: &str, new_state: zinc_proto::AgentState) -> bool {
        if let Some(agent) = self.agents.iter_mut().find(|a| a.id == id) {
            agent.state = new_state;
            self.sort_agents();
            true
        } else {
            false
        }
    }

    /// Remove an agent by ID. Returns true if it was found.
    pub fn remove_agent(&mut self, id: &str) -> bool {
        let before = self.agents.len();
        self.agents.retain(|a| a.id != id);
        self.clamp_selection();
        self.agents.len() != before
    }

    /// Set a transient status message that expires after `duration`.
    pub fn set_status(&mut self, msg: String, duration: std::time::Duration) {
        self.status = Some((msg, Instant::now() + duration));
    }

    /// Get the current status message, clearing it if expired.
    pub fn status_message(&mut self) -> Option<&str> {
        if let Some((_, expiry)) = &self.status {
            if Instant::now() >= *expiry {
                self.status = None;
                return None;
            }
        }
        self.status.as_ref().map(|(msg, _)| msg.as_str())
    }
}
