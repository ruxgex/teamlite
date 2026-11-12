//! Teams message HTML → plain text with styled runs. Small tag scanner, no DOM: messages are short
//! and come from a handful of producers (the Teams composer, Outlook, bots).

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Run {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub strike: bool,
    pub mention: bool,
    pub link: Option<String>,
}

#[derive(Default)]
struct Style {
    bold: u32,
    italic: u32,
    code: u32,
    strike: u32,
    mention: u32,
    link: Vec<Option<String>>,
}

pub struct Rendered {
    pub runs: Vec<Run>,
    pub has_image: bool,
    /// Image sources in the message (inline pictures, GIFs), in order.
    pub images: Vec<String>,
    pub quoted: Option<Quote>,
}

/// The message a reply quotes. Teams puts the author in `<strong itemprop="mri" itemid="8:orgid:…">`,
/// but some clients leave its text as the literal placeholder "Display Name", so the MRI and the
/// quoted message id are kept for the UI to resolve the real name.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Quote {
    pub author: String,
    pub author_mri: Option<String>,
    pub message_id: Option<String>,
    pub text: String,
}

impl Rendered {
    pub fn plain(&self) -> String {
        self.runs.iter().map(|r| r.text.as_str()).collect()
    }
}

fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find(name) {
        let i = from + i;
        let before_ok = i == 0 || lower.as_bytes()[i - 1].is_ascii_whitespace();
        let rest = &tag[i + name.len()..];
        let rest_trim = rest.trim_start();
        if before_ok && rest_trim.starts_with('=') {
            let v = rest_trim[1..].trim_start();
            let (q, body) = match v.chars().next() {
                Some(q @ ('"' | '\'')) => (Some(q), &v[1..]),
                _ => (None, v),
            };
            let end = match q {
                Some(q) => body.find(q).unwrap_or(body.len()),
                None => body.find(|c: char| c.is_whitespace() || c == '>').unwrap_or(body.len()),
            };
            return Some(html_escape::decode_html_entities(&body[..end]).into_owned());
        }
        from = i + name.len();
    }
    None
}

pub fn render(html: &str) -> Rendered {
    let mut runs: Vec<Run> = Vec::new();
    let mut st = Style::default();
    let mut has_image = false;
    let mut images: Vec<String> = Vec::new();
    let mut quoted = None;
    let mut skip_depth = 0u32; // inside <style>/<script>/quoted reply blockquote
    let mut quote_buf = String::new();
    let mut quote = Quote::default();
    let mut in_quote = 0u32;
    // Inside the quote: 1 = author <strong>, 2 = timestamp <span> (dropped).
    let mut quote_part: Option<(u8, u32)> = None;
    let mut list_stack: Vec<Option<u32>> = Vec::new();
    let mut spans: Vec<bool> = Vec::new();

    let push = |runs: &mut Vec<Run>, st: &Style, text: &str| {
        if text.is_empty() {
            return;
        }
        let run = Run {
            text: String::new(),
            bold: st.bold > 0,
            italic: st.italic > 0,
            code: st.code > 0,
            strike: st.strike > 0,
            mention: st.mention > 0,
            link: st.link.iter().rev().find_map(|l| l.clone()),
        };
        match runs.last_mut() {
            Some(last) if Run { text: String::new(), ..last.clone() } == run => last.text.push_str(text),
            _ => runs.push(Run { text: text.to_string(), ..run }),
        }
    };
    let newline = |runs: &mut Vec<Run>, st: &Style| {
        let ends_nl = runs.last().is_none_or(|r| r.text.ends_with('\n'));
        if !ends_nl {
            push(runs, st, "\n");
        }
    };

    let mut rest = html;
    while !rest.is_empty() {
        if let Some(stripped) = rest.strip_prefix('<') {
            let end = stripped.find('>').map(|e| e + 1).unwrap_or(rest.len() - 1);
            let tag = &stripped[..end.saturating_sub(1).min(stripped.len())];
            rest = &rest[(end + 1).min(rest.len())..];
            let closing = tag.starts_with('/');
            let name: String = tag
                .trim_start_matches('/')
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect::<String>()
                .to_ascii_lowercase();
            if matches!(name.as_str(), "style" | "script" | "head") {
                if closing { skip_depth = skip_depth.saturating_sub(1) } else { skip_depth += 1 }
                continue;
            }
            if skip_depth > 0 {
                continue;
            }
            // Replies embed the quoted message as <blockquote itemtype=".../Reply">.
            if name == "blockquote" {
                if closing {
                    if in_quote > 0 {
                        in_quote -= 1;
                        if in_quote == 0 {
                            let mut q = std::mem::take(&mut quote);
                            q.author = q.author.split_whitespace().collect::<Vec<_>>().join(" ");
                            q.text = quote_buf.split_whitespace().collect::<Vec<_>>().join(" ");
                            quoted = Some(q);
                            quote_buf.clear();
                            quote_part = None;
                        }
                    }
                } else if in_quote > 0 {
                    in_quote += 1;
                } else if attr(tag, "itemtype").is_some_and(|t| t.contains("Reply")) {
                    in_quote = 1;
                    quote = Quote { message_id: attr(tag, "itemid").filter(|i| !i.is_empty()), ..Quote::default() };
                }
                continue;
            }
            if in_quote > 0 {
                let prop = if closing { None } else { attr(tag, "itemprop") };
                match (&mut quote_part, closing) {
                    (Some((_, depth)), false) if !tag.ends_with('/') && !matches!(name.as_str(), "br" | "img") => *depth += 1,
                    (Some((_, depth)), true) => {
                        *depth -= 1;
                        if *depth == 0 {
                            quote_part = None;
                        }
                    }
                    (None, false)
                        if prop.as_deref() == Some("mri")
                            || (name == "strong" && quote.author.is_empty() && quote_buf.trim().is_empty()) =>
                    {
                        quote.author_mri = attr(tag, "itemid").filter(|i| !i.is_empty());
                        quote_part = Some((1, 1));
                    }
                    (None, false) if prop.as_deref() == Some("time") => quote_part = Some((2, 1)),
                    _ => {}
                }
                if quote_part.is_none() && matches!(name.as_str(), "p" | "div" | "br") {
                    quote_buf.push(' ');
                }
                continue;
            }
            match (name.as_str(), closing) {
                ("b" | "strong", false) => st.bold += 1,
                ("b" | "strong", true) => st.bold = st.bold.saturating_sub(1),
                ("i" | "em", false) => st.italic += 1,
                ("i" | "em", true) => st.italic = st.italic.saturating_sub(1),
                ("s" | "strike" | "del", false) => st.strike += 1,
                ("s" | "strike" | "del", true) => st.strike = st.strike.saturating_sub(1),
                ("code" | "pre", false) => st.code += 1,
                ("code" | "pre", true) => st.code = st.code.saturating_sub(1),
                ("h1" | "h2" | "h3" | "h4", false) => {
                    newline(&mut runs, &st);
                    st.bold += 1
                }
                ("h1" | "h2" | "h3" | "h4", true) => {
                    st.bold = st.bold.saturating_sub(1);
                    newline(&mut runs, &st)
                }
                ("a", false) => st.link.push(attr(tag, "href")),
                ("a", true) => {
                    st.link.pop();
                }
                ("span", false) => {
                    let mention = attr(tag, "itemtype").is_some_and(|t| t.contains("Mention"));
                    st.mention += mention as u32;
                    spans.push(mention);
                }
                ("span", true) => {
                    if spans.pop() == Some(true) {
                        st.mention = st.mention.saturating_sub(1);
                    }
                }
                ("br", _) => push(&mut runs, &st, "\n"),
                ("p" | "div" | "tr", _) => newline(&mut runs, &st),
                ("ul", false) => list_stack.push(None),
                ("ol", false) => list_stack.push(Some(0)),
                ("ul" | "ol", true) => {
                    list_stack.pop();
                    newline(&mut runs, &st)
                }
                ("li", false) => {
                    newline(&mut runs, &st);
                    let depth = list_stack.len().saturating_sub(1);
                    let bullet = match list_stack.last_mut() {
                        Some(Some(n)) => {
                            *n += 1;
                            format!("{}{}. ", "  ".repeat(depth), n)
                        }
                        _ => format!("{}• ", "  ".repeat(depth)),
                    };
                    push(&mut runs, &st, &bullet);
                }
                ("td", true) => push(&mut runs, &st, "  "),
                ("img", _) => {
                    let emoji = attr(tag, "itemtype").is_some_and(|t| t.contains("Emoji"))
                        || attr(tag, "itemscope").is_some_and(|t| t == "emoji");
                    match attr(tag, "alt").filter(|a| emoji && !a.is_empty()) {
                        Some(alt) => push(&mut runs, &st, &alt),
                        None => {
                            has_image = true;
                            match attr(tag, "src").filter(|s| s.starts_with("https://")) {
                                Some(src) => images.push(src),
                                None => push(&mut runs, &st, "🖼 image"),
                            }
                        }
                    }
                }
                ("emoji", false) => {
                    if let Some(alt) = attr(tag, "alt") {
                        push(&mut runs, &st, &alt);
                    }
                }
                _ => {}
            }
        } else {
            let end = rest.find('<').unwrap_or(rest.len());
            let raw = &rest[..end];
            rest = &rest[end..];
            if skip_depth > 0 {
                continue;
            }
            let decoded = html_escape::decode_html_entities(raw);
            let mut text = String::with_capacity(decoded.len());
            let code = st.code > 0;
            let mut prev_space = runs.last().is_none_or(|r| r.text.ends_with([' ', '\n']));
            for ch in decoded.chars() {
                let ch = if ch == '\u{a0}' { ' ' } else { ch };
                if !code && (ch == '\n' || ch == '\r' || ch == '\t' || ch == ' ') {
                    if !prev_space {
                        text.push(' ');
                        prev_space = true;
                    }
                } else {
                    text.push(ch);
                    prev_space = ch == '\n';
                }
            }
            if in_quote > 0 {
                match quote_part {
                    Some((1, _)) => quote.author.push_str(&text),
                    Some(_) => {}
                    None => quote_buf.push_str(&text),
                }
            } else {
                push(&mut runs, &st, &text);
            }
        }
    }
    // Trim trailing whitespace/newlines.
    while let Some(last) = runs.last_mut() {
        let t = last.text.trim_end().to_string();
        if t.is_empty() {
            runs.pop();
        } else {
            last.text = t;
            break;
        }
    }
    if let Some(first) = runs.first_mut() {
        first.text = first.text.trim_start().to_string();
    }
    Rendered { runs, has_image, images, quoted }
}

/// Plain text → HTML for sending. Keeps line breaks; turns URLs into links.
pub fn to_html(text: &str) -> String {
    let mut out = String::new();
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push_str("<br>");
        }
        for (j, word) in line.split(' ').enumerate() {
            if j > 0 {
                out.push(' ');
            }
            let esc = html_escape::encode_text(word);
            if word.starts_with("https://") || word.starts_with("http://") {
                let href = html_escape::encode_double_quoted_attribute(word);
                out.push_str(&format!("<a href=\"{href}\">{esc}</a>"));
            } else {
                out.push_str(&esc);
            }
        }
    }
    format!("<p>{out}</p>")
}

/// One line for chat list previews.
pub fn preview(html: &str) -> String {
    let r = render(html);
    let p = r.plain().split_whitespace().collect::<Vec<_>>().join(" ");
    if p.is_empty() && r.has_image { "🖼 image".into() } else { p }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_formatting() {
        let r = render("<p>Hello <b>world</b> &amp; <i>you</i></p><p>next</p>");
        assert_eq!(r.plain(), "Hello world & you\nnext");
        assert!(r.runs.iter().any(|x| x.bold && x.text == "world"));
        assert!(r.runs.iter().any(|x| x.italic && x.text == "you"));
    }

    #[test]
    fn mentions_links_emoji() {
        let r = render(r#"<p><span itemscope="" itemtype="http://schema.skype.com/Mention" itemid="0">Ann</span> see <a href="https://x.y/z">this</a> <img itemtype="http://schema.skype.com/Emoji" alt="😀" src="x"></p>"#);
        assert_eq!(r.plain(), "Ann see this 😀");
        assert!(r.runs.iter().any(|x| x.mention && x.text == "Ann"));
        assert!(r.runs.iter().any(|x| x.link.as_deref() == Some("https://x.y/z")));
    }

    #[test]
    fn reply_quote_is_separated() {
        let r = render(r#"<blockquote itemscope itemtype="http://schema.skype.com/Reply"><strong>Bob</strong><p>old</p></blockquote><p>new</p>"#);
        assert_eq!(r.plain(), "new");
        let q = r.quoted.unwrap();
        assert_eq!((q.author.as_str(), q.text.as_str()), ("Bob", "old"));
    }

    #[test]
    fn reply_quote_keeps_author_mri_and_drops_time() {
        let r = render(r#"<blockquote itemscope="" itemtype="http://schema.skype.com/Reply" itemid="1762979000000"><strong itemprop="mri" itemid="8:orgid:abc">Display Name</strong><span itemprop="time" itemid="1762979000000"></span><p itemprop="preview">Non vanno le chiamate</p></blockquote><p>Questo</p>"#);
        assert_eq!(r.plain(), "Questo");
        let q = r.quoted.unwrap();
        assert_eq!(q.author, "Display Name");
        assert_eq!(q.author_mri.as_deref(), Some("8:orgid:abc"));
        assert_eq!(q.message_id.as_deref(), Some("1762979000000"));
        assert_eq!(q.text, "Non vanno le chiamate");
    }

    #[test]
    fn images_are_collected() {
        let r = render(r#"<p>look</p><img itemtype="http://schema.skype.com/AMSImage" src="https://eu-api.asm.skype.com/v1/objects/0-x/views/imgo" width="250">"#);
        assert_eq!(r.plain(), "look");
        assert_eq!(r.images, vec!["https://eu-api.asm.skype.com/v1/objects/0-x/views/imgo".to_string()]);
    }

    #[test]
    fn lists_and_plain() {
        assert_eq!(render("<ul><li>a</li><li>b</li></ul>").plain(), "• a\n• b");
        assert_eq!(render("just text").plain(), "just text");
    }

    #[test]
    fn outgoing_html() {
        assert_eq!(to_html("a<b\nhttps://x.y"), "<p>a&lt;b<br><a href=\"https://x.y\">https://x.y</a></p>");
    }
}
