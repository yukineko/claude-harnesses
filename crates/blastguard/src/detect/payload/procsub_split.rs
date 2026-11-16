//! The statement splitter of the process-substitution reading in
//! [`super::stdin_exec_verdict`]: moved out of `payload.rs` unchanged (file
//! growth cap, backlog 7671c79f). Its behaviour is that of the original.

/// `cmd` split into simple command stages at `;`, newlines, `|`, `|&`, `||`,
/// `&&` and a background `&` outside quotes and parentheses — but NOT at the
/// `&` of a redirection (`<&3`, `2>&1`, `&>f`), which belongs to its stage
/// (`split_statements_into_pipe_stages` splits there, which cut
/// `bash <&3 3< <(…)` into `bash <` and `3 3< <(…)`).
pub(super) fn procsub_stages(cmd: &str) -> Vec<String> {
    let chars: Vec<char> = cmd.chars().collect();
    let mut stages = Vec::new();
    let mut cur = String::new();
    let (mut in_s, mut in_d) = (false, false);
    let mut depth: u32 = 0;
    let mut k = 0;
    while k < chars.len() {
        let c = chars[k];
        if c == '\\' && !in_s {
            cur.push(c);
            if let Some(&n) = chars.get(k + 1) {
                cur.push(n);
            }
            k += 2;
            continue;
        }
        if c == '\'' && !in_d {
            in_s = !in_s;
        } else if c == '"' && !in_s {
            in_d = !in_d;
        } else if !in_s && !in_d {
            match c {
                '(' => depth += 1,
                ')' => depth = depth.saturating_sub(1),
                _ => {}
            }
            let redirect_amp = c == '&'
                && (matches!(k.checked_sub(1).map(|p| chars[p]), Some('<' | '>'))
                    || chars.get(k + 1) == Some(&'>'));
            if depth == 0 && matches!(c, ';' | '\n' | '|' | '&') && !redirect_amp {
                stages.push(std::mem::take(&mut cur));
                k += 1;
                continue;
            }
        }
        cur.push(c);
        k += 1;
    }
    stages.push(cur);
    stages.retain(|s| !s.trim().is_empty());
    stages
}
