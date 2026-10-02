/// Minimal HTML escaping for text we splice into HTML ourselves (templates
/// escape automatically; this is for the few places that build HTML in Rust).
pub fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Human-friendly "in 2 days" / "3 hours ago" style durations.
pub fn humanize_duration(seconds: i64) -> String {
    let s = seconds.abs();
    let (n, unit) = if s < 60 {
        return "less than a minute".into();
    } else if s < 3600 {
        (s / 60, "minute")
    } else if s < 86_400 {
        (s / 3600, "hour")
    } else {
        (s / 86_400, "day")
    };
    format!("{n} {unit}{}", if n == 1 { "" } else { "s" })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes() {
        assert_eq!(escape_html(r#"<a href="x">'&'</a>"#), "&lt;a href=&quot;x&quot;&gt;&#39;&amp;&#39;&lt;/a&gt;");
    }

    #[test]
    fn humanizes() {
        assert_eq!(humanize_duration(30), "less than a minute");
        assert_eq!(humanize_duration(120), "2 minutes");
        assert_eq!(humanize_duration(3600), "1 hour");
        assert_eq!(humanize_duration(3 * 86_400 + 5), "3 days");
    }
}
