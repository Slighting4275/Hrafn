//! Turning raw page events from the recorder into clean steps.

use serde_json::Value;

/// Add a recorded step. Keystrokes into the same field collapse into one `type`
/// with the final text, and a click into a field right before typing in it is
/// dropped (typing focuses the field anyway on replay).
pub fn record_step(steps: &mut Vec<Value>, step: Value) {
    if step["type"] == "type" {
        // First drop a click into the same field, then merge with typing before it
        // (type, click to move the cursor, type more = one step).
        if let Some(last) = steps.last()
            && last["type"] == "click"
            && last["target"] == step["target"]
        {
            steps.pop();
        }
        if let Some(last) = steps.last_mut()
            && last["type"] == "type"
            && last["target"] == step["target"]
        {
            *last = step;
            return;
        }
    }
    // The page recorder reports only the first click of a double-click.
    if step["type"] == "dblclick"
        && steps.last().is_some_and(|l| l["type"] == "click" && l["target"] == step["target"])
    {
        steps.pop();
    }
    steps.push(step);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run(events: &[Value]) -> Vec<Value> {
        let mut steps = Vec::new();
        for e in events {
            record_step(&mut steps, e.clone());
        }
        steps
    }

    #[test]
    fn typing_collapses_and_focus_click_is_dropped() {
        let steps = run(&[
            json!({"type": "click", "target": "#q"}),
            json!({"type": "type", "target": "#q", "text": "r"}),
            json!({"type": "type", "target": "#q", "text": "ra"}),
            json!({"type": "type", "target": "#q", "text": "raven"}),
            json!({"type": "keys", "combo": "Enter"}),
            json!({"type": "type", "target": "#q", "text": "again"}),
        ]);
        assert_eq!(
            steps,
            vec![
                json!({"type": "type", "target": "#q", "text": "raven"}),
                json!({"type": "keys", "combo": "Enter"}),
                json!({"type": "type", "target": "#q", "text": "again"}),
            ]
        );
    }

    #[test]
    fn type_click_type_in_one_field_is_one_step() {
        let steps = run(&[
            json!({"type": "type", "target": "#q", "text": "rav"}),
            json!({"type": "click", "target": "#q"}),
            json!({"type": "type", "target": "#q", "text": "ravens"}),
        ]);
        assert_eq!(steps, vec![json!({"type": "type", "target": "#q", "text": "ravens"})]);
    }

    #[test]
    fn double_click_replaces_its_single_clicks() {
        let steps = run(&[
            json!({"type": "click", "target": "#a"}),
            json!({"type": "click", "target": "#b"}),
            json!({"type": "dblclick", "target": "#b"}),
        ]);
        assert_eq!(steps, vec![json!({"type": "click", "target": "#a"}), json!({"type": "dblclick", "target": "#b"})]);
    }

    #[test]
    fn earlier_click_on_the_same_element_survives_double_click() {
        let steps = run(&[
            json!({"type": "click", "target": "#a"}),
            json!({"type": "click", "target": "#a"}),
            json!({"type": "dblclick", "target": "#a"}),
        ]);
        assert_eq!(steps, vec![json!({"type": "click", "target": "#a"}), json!({"type": "dblclick", "target": "#a"})]);
    }
}
