// SPDX-License-Identifier: GPL-3.0-or-later
//! Task Scheduler XML (schema 1.2) for the tasks the installer registers
//! with `schtasks.exe /Create /XML`. The file must be written as UTF-16 LE
//! with a BOM (see [`to_utf16_file`]); schtasks rejects some UTF-8 files.

/// Folder + name of the elevated index helper task.
pub const INDEX_HELPER_TASK: &str = r"\explorer-rs\IndexHelper";
/// Name used by scripts/install-index-helper.ps1 before the installer
/// existed; removed on install so only one helper task remains.
pub const LEGACY_INDEX_HELPER_TASK: &str = r"\explorer-rs\index helper";
/// One-shot task used to start a program as the interactive user, without
/// elevation, when Windows Explorer is not running (see `win::launch`).
pub const LAUNCH_TASK: &str = r"\explorer-rs\Launch";

/// Escape text for XML element content and attribute values.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // Control characters are not valid in XML 1.0; they cannot occur
            // in Windows paths, so drop them defensively.
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => {}
            c => out.push(c),
        }
    }
    out
}

/// Common `<Settings>` block. `priority`: 4 = normal, 7 = below normal
/// (the Task Scheduler default, also lowers I/O priority).
fn settings(restart_on_failure: bool, priority: u8) -> String {
    let restart = if restart_on_failure {
        "    <RestartOnFailure>\n      <Interval>PT1M</Interval>\n      <Count>3</Count>\n    </RestartOnFailure>\n"
    } else {
        ""
    };
    format!(
        "  <Settings>\n\
         \x20   <AllowStartOnDemand>true</AllowStartOnDemand>\n\
         {restart}\
         \x20   <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>\n\
         \x20   <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>\n\
         \x20   <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>\n\
         \x20   <AllowHardTerminate>true</AllowHardTerminate>\n\
         \x20   <StartWhenAvailable>false</StartWhenAvailable>\n\
         \x20   <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>\n\
         \x20   <IdleSettings>\n\
         \x20     <StopOnIdleEnd>false</StopOnIdleEnd>\n\
         \x20     <RestartOnIdle>false</RestartOnIdle>\n\
         \x20   </IdleSettings>\n\
         \x20   <Enabled>true</Enabled>\n\
         \x20   <Hidden>false</Hidden>\n\
         \x20   <RunOnlyIfIdle>false</RunOnlyIfIdle>\n\
         \x20   <WakeToRun>false</WakeToRun>\n\
         \x20   <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>\n\
         \x20   <Priority>{priority}</Priority>\n\
         \x20 </Settings>\n"
    )
}

struct Spec<'a> {
    uri: &'a str,
    description: &'a str,
    triggers: &'a str,
    user_sid: &'a str,
    run_level: &'a str,
    settings: &'a str,
    command: &'a str,
    arguments: Option<&'a str>,
    work_dir: &'a str,
}

fn task(s: Spec<'_>) -> String {
    let args = s
        .arguments
        .map(|a| format!("      <Arguments>{}</Arguments>\n", escape(a)))
        .unwrap_or_default();
    let (triggers, run_level, settings) = (s.triggers, s.run_level, s.settings);
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\n\
         <Task version=\"1.2\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">\n\
         \x20 <RegistrationInfo>\n\
         \x20   <Author>explorer.rs</Author>\n\
         \x20   <Description>{description}</Description>\n\
         \x20   <URI>{uri}</URI>\n\
         \x20 </RegistrationInfo>\n\
         {triggers}\
         \x20 <Principals>\n\
         \x20   <Principal id=\"Author\">\n\
         \x20     <UserId>{sid}</UserId>\n\
         \x20     <LogonType>InteractiveToken</LogonType>\n\
         \x20     <RunLevel>{run_level}</RunLevel>\n\
         \x20   </Principal>\n\
         \x20 </Principals>\n\
         {settings}\
         \x20 <Actions Context=\"Author\">\n\
         \x20   <Exec>\n\
         \x20     <Command>{command}</Command>\n\
         {args}\
         \x20     <WorkingDirectory>{work_dir}</WorkingDirectory>\n\
         \x20   </Exec>\n\
         \x20 </Actions>\n\
         </Task>\n",
        description = escape(s.description),
        uri = escape(s.uri),
        sid = escape(s.user_sid),
        command = escape(s.command),
        work_dir = escape(s.work_dir),
    )
}

/// Elevated index helper: starts at the interactive user's logon, runs with
/// highest privileges, never times out, restarts 3 times (1 min apart) on
/// failure. Mirrors scripts/install-index-helper.ps1 (ADR 0010).
pub fn index_helper_task(user_sid: &str, helper_exe: &str, work_dir: &str) -> String {
    let triggers = format!(
        "  <Triggers>\n\
         \x20   <LogonTrigger>\n\
         \x20     <Enabled>true</Enabled>\n\
         \x20     <UserId>{}</UserId>\n\
         \x20   </LogonTrigger>\n\
         \x20 </Triggers>\n",
        escape(user_sid)
    );
    task(Spec {
        uri: INDEX_HELPER_TASK,
        description: "explorer.rs: elevated NTFS MFT/USN reader for the file-name index (installed by explorer-rs-setup.exe)",
        triggers: &triggers,
        user_sid,
        run_level: "HighestAvailable",
        settings: &settings(true, 7),
        command: helper_exe,
        arguments: None,
        work_dir,
    })
}

/// On-demand task (no trigger) that starts `command arguments` as the
/// interactive user with a filtered, non-elevated token.
pub fn launch_task(user_sid: &str, command: &str, arguments: &str, work_dir: &str) -> String {
    task(Spec {
        uri: LAUNCH_TASK,
        description:
            "explorer.rs: starts explorer.rs or Windows Explorer for the signed-in user after setup",
        triggers: "",
        user_sid,
        run_level: "LeastPrivilege",
        settings: &settings(false, 4),
        command,
        arguments: Some(arguments).filter(|a| !a.is_empty()),
        work_dir,
    })
}

/// UTF-16 LE with BOM, CRLF line endings: the encoding schtasks expects.
pub fn to_utf16_file(xml: &str) -> Vec<u8> {
    let mut out = vec![0xFF, 0xFE];
    for unit in xml
        .replace("\r\n", "\n")
        .replace('\n', "\r\n")
        .encode_utf16()
    {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal well-formedness check: balanced tags, no stray '&' or '<'.
    fn assert_well_formed(xml: &str) {
        let body = xml.split_once("?>").expect("xml declaration").1;
        let mut stack: Vec<String> = Vec::new();
        let mut rest = body;
        while let Some(start) = rest.find('<') {
            let text = &rest[..start];
            assert_entities_ok(text);
            let end = rest[start..].find('>').expect("unterminated tag") + start;
            let tag = &rest[start + 1..end];
            if let Some(name) = tag.strip_prefix('/') {
                assert_eq!(stack.pop().as_deref(), Some(name), "mismatched </{name}>");
            } else if !tag.ends_with('/') {
                let name = tag.split_whitespace().next().unwrap().to_string();
                assert_entities_ok(tag);
                stack.push(name);
            }
            rest = &rest[end + 1..];
        }
        assert!(stack.is_empty(), "unclosed: {stack:?}");
    }

    fn assert_entities_ok(text: &str) {
        for (i, _) in text.match_indices('&') {
            let ent = &text[i..];
            assert!(
                ["&amp;", "&lt;", "&gt;", "&quot;", "&apos;"]
                    .iter()
                    .any(|e| ent.starts_with(e)),
                "bare & in {text:?}"
            );
        }
    }

    #[test]
    fn helper_task_contents() {
        let x = index_helper_task(
            "S-1-5-21-1-2-3-1001",
            r"C:\Program Files\explorer-rs\explorer-rs-index-helper.exe",
            r"C:\Program Files\explorer-rs",
        );
        assert_well_formed(&x);
        assert!(x.contains("<RunLevel>HighestAvailable</RunLevel>"));
        assert!(x.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"));
        assert!(x.contains("<Interval>PT1M</Interval>"));
        assert!(x.contains("<Count>3</Count>"));
        assert!(x.contains("<LogonTrigger>"));
        assert_eq!(x.matches("S-1-5-21-1-2-3-1001").count(), 2);
        assert!(x.contains(r"<URI>\explorer-rs\IndexHelper</URI>"));
        assert!(!x.contains("<Arguments>"));
    }

    #[test]
    fn launch_task_contents() {
        let x = launch_task(
            "S-1-5-21-9",
            r"C:\Program Files\explorer-rs\explorer-rs.exe",
            "--dev",
            r"C:\Program Files\explorer-rs",
        );
        assert_well_formed(&x);
        assert!(x.contains("<RunLevel>LeastPrivilege</RunLevel>"));
        assert!(x.contains("<Arguments>--dev</Arguments>"));
        assert!(!x.contains("<Triggers>"));
        assert!(!x.contains("RestartOnFailure"));
        let y = launch_task("S-1", r"C:\Windows\explorer.exe", "", r"C:\Windows");
        assert!(!y.contains("<Arguments>"));
    }

    #[test]
    fn paths_with_special_characters_are_escaped() {
        let dir = r#"C:\Tools & "Stuff" <x> 'y'"#;
        let x = index_helper_task("S-1-5-21-1", &format!(r"{dir}\helper.exe"), dir);
        assert_well_formed(&x);
        assert!(x.contains(r"C:\Tools &amp; &quot;Stuff&quot; &lt;x&gt; &apos;y&apos;\helper.exe"));
        assert!(!x.contains("Tools & "));
        assert_eq!(escape("a\u{1}b"), "ab");
    }

    #[test]
    fn utf16_encoding() {
        let b = to_utf16_file("<a>\n</a>");
        assert_eq!(&b[..2], &[0xFF, 0xFE]);
        let units: Vec<u16> = b[2..]
            .chunks(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(String::from_utf16(&units).unwrap(), "<a>\r\n</a>");
    }
}
