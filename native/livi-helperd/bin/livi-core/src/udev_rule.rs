use std::path::Path;

use crate::privileged::Privileged;

const RULE_FILE: &str = "/etc/udev/rules.d/99-LIVI.rules";
const TEMPLATE: &str = "99-LIVI.rules.template";
/// The rule calls it to tell a real mouse from a touch panel's mouse interface.
const TOUCH_FILTER: &str = "livi-touch-filter";

fn version_line(template: &str) -> String {
    template
        .lines()
        .find(|l| {
            l.strip_prefix("# LIVI-RULE-VERSION=")
                .is_some_and(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
        })
        .unwrap_or("# LIVI-RULE-VERSION=0")
        .to_string()
}

fn current(template: &str) -> bool {
    std::fs::read_to_string(RULE_FILE).is_ok_and(|rule| rule.contains(&version_line(template)))
}

/// True when the rule was installed now, LIVI starts again to pick it up.
pub async fn ensure(p: &Privileged) -> bool {
    if !cfg!(target_os = "linux") {
        return false;
    }
    let template = match std::fs::read_to_string(p.templates.join(TEMPLATE)) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("[udev] {TEMPLATE} does not read: {e}");
            return false;
        }
    };
    if Path::new(RULE_FILE).exists() && current(&template) {
        return false;
    }
    let filter = std::fs::read_to_string(p.templates.join(TOUCH_FILTER)).ok();
    let mut files = vec![("rule", template.as_str())];
    if let Some(filter) = filter.as_deref() {
        files.push(("filter", filter));
    }
    if !p.helper_installs("install-udev-rule", &files).await {
        eprintln!("[udev] {RULE_FILE} not installed, run the LIVI install script");
        return false;
    }
    // A rule that still does not match would start LIVI again and again.
    let installed = current(&template);
    if installed {
        println!("[udev] {RULE_FILE} installed, starting again to pick it up");
    }
    installed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_version_comes_from_its_own_line() {
        let t = "# LIVI udev rules\n# LIVI-RULE-VERSION=6\nSUBSYSTEM==\"usb\"\n";
        assert_eq!(version_line(t), "# LIVI-RULE-VERSION=6");
        assert_eq!(version_line("# LIVI-RULE-VERSION=x\n"), "# LIVI-RULE-VERSION=0");
        assert_eq!(version_line(""), "# LIVI-RULE-VERSION=0");
    }
}
