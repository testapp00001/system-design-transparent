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

/// Human-friendly "2 days 5 hours" / "3 hours" style durations.
pub fn humanize_duration(seconds: i64) -> String {
    fn unit(n: i64, name: &str) -> String {
        format!("{n} {name}{}", if n == 1 { "" } else { "s" })
    }
    let s = seconds.abs();
    match s {
        0..60 => "less than a minute".into(),
        60..3600 => unit(s / 60, "minute"),
        3600..86_400 => unit(s / 3600, "hour"),
        _ => {
            let (days, hours) = (s / 86_400, s % 86_400 / 3600);
            // Hours only matter while the number of days is small.
            if hours == 0 || days >= 7 {
                unit(days, "day")
            } else {
                format!("{} {}", unit(days, "day"), unit(hours, "hour"))
            }
        }
    }
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
        assert_eq!(humanize_duration(2 * 86_400 + 23 * 3600 + 59), "2 days 23 hours");
        assert_eq!(humanize_duration(86_400 + 3600), "1 day 1 hour");
        assert_eq!(humanize_duration(9 * 86_400 + 3600), "9 days");
    }
}
