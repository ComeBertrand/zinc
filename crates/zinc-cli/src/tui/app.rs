use std::path::PathBuf;
use std::time::Instant;

use zinc_proto::AgentInfo;

pub enum Mode {
    Normal,
    SpawnPickProject(PickerState),
    SpawnEnterPath(String),
    SpawnPickSession { dir: PathBuf, picker: PickerState },
}

pub struct PickerItem {
    pub display: String,
    pub id: String,
}

pub struct PickerState {
    pub title: String,
    pub items: Vec<PickerItem>,
    pub filter: String,
    pub selected: usize,
}

impl PickerState {
    pub fn new(title: impl Into<String>, items: Vec<PickerItem>) -> Self {
        Self {
            title: title.into(),
            items,
            filter: String::new(),
            selected: 0,
        }
    }

    pub fn filtered_items(&self) -> Vec<&PickerItem> {
        if self.filter.is_empty() {
            self.items.iter().collect()
        } else {
            let lower = self.filter.to_lowercase();
            self.items
                .iter()
                .filter(|item| item.display.to_lowercase().contains(&lower))
                .collect()
        }
    }

    pub fn selected_item(&self) -> Option<&PickerItem> {
        let filtered = self.filtered_items();
        filtered.get(self.selected).copied()
    }

    pub fn select_next(&mut self) {
        let count = self.filtered_items().len();
        if count > 0 {
            self.selected = (self.selected + 1).min(count - 1);
        }
    }

    pub fn select_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn type_char(&mut self, c: char) {
        self.filter.push(c);
        self.selected = 0;
    }

    pub fn backspace(&mut self) {
        self.filter.pop();
        self.selected = 0;
    }
}

pub struct App {
    pub agents: Vec<AgentInfo>,
    pub selected: usize,
    /// Transient status message (errors, confirmations) with expiry time.
    pub status: Option<(String, Instant)>,
    pub mode: Mode,
    /// Peek preview content (stripped scrollback text). Some = peek active.
    pub peek: Option<String>,
    /// Filter text for the agent list. Empty = show all.
    pub filter: String,
    /// Whether the user is currently typing into the filter.
    pub filter_active: bool,
}

impl App {
    pub fn new() -> Self {
        Self {
            agents: Vec::new(),
            selected: 0,
            status: None,
            mode: Mode::Normal,
            peek: None,
            filter: String::new(),
            filter_active: false,
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

    /// Return agents matching the current filter.
    pub fn visible_agents(&self) -> Vec<&AgentInfo> {
        if self.filter.is_empty() {
            self.agents.iter().collect()
        } else {
            let lower = self.filter.to_lowercase();
            self.agents
                .iter()
                .filter(|a| {
                    a.id.to_lowercase().contains(&lower)
                        || a.provider.to_lowercase().contains(&lower)
                        || a.dir.to_string_lossy().to_lowercase().contains(&lower)
                })
                .collect()
        }
    }

    pub fn select_next(&mut self) {
        let count = self.visible_agents().len();
        if count > 0 {
            self.selected = (self.selected + 1).min(count - 1);
        }
    }

    pub fn select_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn selected_agent(&self) -> Option<&AgentInfo> {
        self.visible_agents().get(self.selected).copied()
    }

    fn clamp_selection(&mut self) {
        let count = self.visible_agents().len();
        if count == 0 {
            self.selected = 0;
        } else {
            self.selected = self.selected.min(count - 1);
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

    /// Update a single agent's context usage percentage.
    pub fn update_context(&mut self, id: &str, context_percent: u8) {
        if let Some(agent) = self.agents.iter_mut().find(|a| a.id == id) {
            agent.context_percent = Some(context_percent);
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
