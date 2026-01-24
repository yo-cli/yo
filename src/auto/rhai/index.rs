//! 时间索引（按小时分桶）

use super::types::Rule;
use chrono::{NaiveTime, Timelike};
use std::collections::HashMap;

pub struct TimeIndex {
    pub tick_rules: HashMap<u32, Vec<usize>>,
    pub unlock_rules: Vec<usize>,
    pub lock_rules: Vec<usize>,
}

impl TimeIndex {
    pub fn build(rules: &[Rule]) -> Self {
        let mut index = Self {
            tick_rules: HashMap::new(),
            unlock_rules: Vec::new(),
            lock_rules: Vec::new(),
        };

        for (i, rule) in rules.iter().enumerate() {
            if !rule.trigger.enabled {
                continue;
            }

            for event in &rule.trigger.events {
                match event.as_str() {
                    "tick" => {
                        let hours = match &rule.trigger.time_range {
                            Some((start, end)) => Self::get_hours_in_range(start, end),
                            None => (0..24).collect(),
                        };
                        for hour in hours {
                            index.tick_rules.entry(hour).or_default().push(i);
                        }
                    }
                    "unlock" => index.unlock_rules.push(i),
                    "lock" => index.lock_rules.push(i),
                    _ => {}
                }
            }
        }
        index
    }

    pub fn get_hours_in_range(start: &str, end: &str) -> Vec<u32> {
        let start_time = NaiveTime::parse_from_str(start, "%H:%M").ok();
        let end_time = NaiveTime::parse_from_str(end, "%H:%M").ok();

        match (start_time, end_time) {
            (Some(s), Some(e)) => {
                let (sh, eh) = (s.hour(), e.hour());
                if sh <= eh {
                    (sh..=eh).collect()
                } else {
                    let mut hours: Vec<u32> = (sh..24).collect();
                    hours.extend(0..=eh);
                    hours
                }
            }
            _ => (0..24).collect(),
        }
    }
}
