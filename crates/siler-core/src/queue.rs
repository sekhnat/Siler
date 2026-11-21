//! Named download queues.

use serde::{Deserialize, Serialize};

use crate::scheduler::Schedule;

/// The queue every download belongs to unless put elsewhere. It can't be deleted.
pub const MAIN_QUEUE: i64 = 1;

/// What happens when a queue has finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AfterDone {
    #[default]
    Nothing,
    Quit,
    Sleep,
    Hibernate,
    PowerOff,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Queue {
    pub id: i64,
    pub name: String,
    /// Downloads from this queue running at once; 0 = only the global limit applies.
    pub max_concurrent: u8,
    pub schedule: Schedule,
    pub after_done: AfterDone,
    /// Started (by hand or by its schedule) and not yet finished. Not saved.
    pub active: bool,
}

impl Default for Queue {
    fn default() -> Self {
        Queue {
            id: 0,
            name: String::new(),
            max_concurrent: 0,
            schedule: Schedule::default(),
            after_done: AfterDone::Nothing,
            active: false,
        }
    }
}

impl Queue {
    pub(crate) fn main() -> Queue {
        Queue {
            id: MAIN_QUEUE,
            name: "Main queue".into(),
            ..Default::default()
        }
    }

    pub(crate) fn normalized(mut self) -> Self {
        self.name = self.name.trim().chars().take(60).collect();
        if self.name.is_empty() {
            self.name = "Queue".into();
        }
        self.max_concurrent = self.max_concurrent.min(20);
        self.schedule = self.schedule.normalized();
        self
    }

    /// The part saved in the `config` column.
    pub(crate) fn config_json(&self) -> String {
        serde_json::json!({
            "max_concurrent": self.max_concurrent,
            "schedule": self.schedule,
            "after_done": self.after_done,
        })
        .to_string()
    }

    pub(crate) fn from_row(id: i64, name: String, config: &str) -> Queue {
        let mut q: Queue = serde_json::from_str(config).unwrap_or_default();
        q.id = id;
        q.name = name;
        q.active = false;
        q
    }
}
