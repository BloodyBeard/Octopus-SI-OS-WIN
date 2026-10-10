import html
import re
from html.parser import HTMLParser


MXID_IN_TEXT_RE = re.compile(r"(?<![\w/])(@[0-9A-Za-z._=/-]+:[0-9A-Za-z.-]+(?::[0-9]{1,5})?)")


class MatrixHtmlSanitizer(HTMLParser):
    ALLOWED = {
        "a",
        "b",
        "blockquote",
        "br",
        "code",
        "del",
        "em",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "hr",
        "i",
        "li",
        "ol",
        "p",
        "pre",
        "s",
        "strike",
        "strong",
        "table",
        "tbody",
        "td",
        "th",
        "thead",
        "tr",
        "u",
        "ul",
    }
    VOID = {"br", "hr"}

    def __init__(self):
        super().__init__(convert_charrefs=False)
        self.parts = []
        self.skip = 0

    def safe_url(self, value):
        value = re.sub(r"[\x00-\x1f\x7f]+", "", value or "").strip()
        match = re.match(r"^([A-Za-z][A-Za-z0-9+.-]*):", value)
        scheme = match.group(1).lower() if match else ""
        if scheme and scheme not in {"http", "https", "matrix", "mailto"}:
            return ""
        return value

    def safe_attrs(self, tag, attrs):
        result = []
        for key, value in attrs:
            key = str(key or "").lower()
            value = "" if value is None else str(value)
            if key.startswith("on"):
                continue
            if tag == "a" and key == "href":
                href = self.safe_url(value)
                if href:
                    result.append(f' href="{html.escape(href, quote=True)}"')
            elif tag == "code" and key == "class" and re.fullmatch(r"language-[0-9A-Za-z_+.-]{1,64}", value):
                result.append(f' class="{html.escape(value, quote=True)}"')
        return "".join(result)

    def handle_starttag(self, tag, attrs):
        tag = tag.lower()
        if tag in {"script", "style"}:
            self.skip += 1
            return
        if self.skip or tag not in self.ALLOWED:
            return
        if tag in self.VOID:
            self.parts.append(f"<{tag}>")
        else:
            self.parts.append(f"<{tag}{self.safe_attrs(tag, attrs)}>")

    def handle_endtag(self, tag):
        tag = tag.lower()
        if tag in {"script", "style"} and self.skip:
            self.skip -= 1
            return
        if not self.skip and tag in self.ALLOWED and tag not in self.VOID:
            self.parts.append(f"</{tag}>")

    def handle_data(self, data):
        if not self.skip:
            self.parts.append(html.escape(data))

    def handle_entityref(self, name):
        if not self.skip:
            self.parts.append(f"&{name};")

    def handle_charref(self, name):
        if not self.skip:
            self.parts.append(f"&#{name};")

    def value(self):
        return "".join(self.parts)


def sanitize_html(value):
    parser = MatrixHtmlSanitizer()
    try:
        parser.feed(value or "")
        parser.close()
        return parser.value()
    except Exception:
        return html.escape(value or "")


def sanitize_markdown_source(value):
    value = re.sub(r"(?is)<\s*(script|style)\b[^>]*>.*?<\s*/\s*\1\s*>", "", value or "")
    value = re.sub(r'''(?is)\s+on[a-z0-9_-]+\s*=\s*("[^"]*"|'[^']*'|[^\s>]+)''', "", value)
    value = re.sub(r'''(?is)\s+(href|src)\s*=\s*("[^"]*(?:javascript|data|vbscript):[^"]*"|'[^']*(?:javascript|data|vbscript):[^']*'|[^\s>]*(?:javascript|data|vbscript):[^\s>]*)''', "", value)
    return value


def safe_link(value):
    value = re.sub(r"[\x00-\x1f\x7f]+", "", value or "").strip()
    scheme = value.split(":", 1)[0].lower() if ":" in value else ""
    if scheme and scheme not in {"http", "https", "matrix", "mailto"}:
        return ""
    return value


def markdown_fallback(value):
    placeholders = []

    def protect(fragment):
        index = len(placeholders)
        placeholders.append(fragment)
        return f"\x00MATRIX{index}\x00"

    value = re.sub(
        r"```([0-9A-Za-z_+.-]*)\n([\s\S]*?)```",
        lambda match: protect(
            f'<pre><code class="language-{html.escape(match.group(1))}">{html.escape(match.group(2))}</code></pre>'
            if match.group(1)
            else f"<pre><code>{html.escape(match.group(2))}</code></pre>"
        ),
        value,
    )
    value = re.sub(r"`([^`\n]+)`", lambda match: protect(f"<code>{html.escape(match.group(1))}</code>"), value)
    value = re.sub(
        r"\[([^\]\n]+)\]\(([^)\n]+)\)",
        lambda match: protect(
            f'<a href="{html.escape(safe_link(match.group(2)), quote=True)}">{html.escape(match.group(1))}</a>'
            if safe_link(match.group(2))
            else html.escape(match.group(1))
        ),
        value,
    )
    parts = re.split(r"(\x00MATRIX[0-9]+\x00)", value)
    for index in range(0, len(parts), 2):
        parts[index] = html.escape(parts[index])
    lines = "".join(parts).split("\n")
    output = []
    index = 0
    while index < len(lines):
        line = lines[index]
        heading = re.match(r"^(#{1,6})\s+(.+)$", line)
        if heading:
            level = len(heading.group(1))
            output.append(f"<h{level}>{heading.group(2)}</h{level}>")
            index += 1
            continue
        if re.match(r"^\s*([-*_])(?:\s*\1){2,}\s*$", line):
            output.append("<hr>")
            index += 1
            continue
        if line.startswith(("&gt; ", "> ")) or line in {"&gt;", ">"}:
            group = []
            while index < len(lines) and (lines[index].startswith(("&gt; ", "> ")) or lines[index] in {"&gt;", ">"}):
                current = lines[index]
                group.append(current[5:] if current.startswith("&gt; ") else current[2:] if current.startswith("> ") else "")
                index += 1
            output.append("<blockquote>" + "<br>".join(group) + "</blockquote>")
            continue
        if re.match(r"^\s*[-*+]\s+", line):
            group = []
            while index < len(lines):
                match = re.match(r"^\s*[-*+]\s+(.+)$", lines[index])
                if not match:
                    break
                group.append(match.group(1))
                index += 1
            output.append("<ul>" + "".join(f"<li>{item}</li>" for item in group) + "</ul>")
            continue
        if re.match(r"^\s*[0-9]+[.)]\s+", line):
            group = []
            while index < len(lines):
                match = re.match(r"^\s*[0-9]+[.)]\s+(.+)$", lines[index])
                if not match:
                    break
                group.append(match.group(1))
                index += 1
            output.append("<ol>" + "".join(f"<li>{item}</li>" for item in group) + "</ol>")
            continue
        output.append(line)
        index += 1
    value = "\n".join(output)
    value = re.sub(r"\*\*([^*\n]+)\*\*", r"<strong>\1</strong>", value)
    value = re.sub(r"__([^_\n]+)__", r"<strong>\1</strong>", value)
    value = re.sub(r"(?<!\*)\*([^*\n]+)\*(?!\*)", r"<em>\1</em>", value)
    value = re.sub(r"(?<!\w)_([^_\n]+)_(?!\w)", r"<em>\1</em>", value)
    value = re.sub(r"~~([^~\n]+)~~", r"<del>\1</del>", value)
    value = value.replace("\n", "<br>\n")
    value = re.sub(r"<br>\n(</?(?:pre|blockquote|h[1-6]|ul|ol|li|hr))", r"\n\1", value)
    value = re.sub(r"(</(?:pre|blockquote|h[1-6]|ul|ol|li)>)<br>", r"\1", value)
    for index, fragment in enumerate(placeholders):
        value = value.replace(f"\x00MATRIX{index}\x00", fragment)
    return value


def markdown_to_html(value):
    value = sanitize_markdown_source(value)
    try:
        import markdown

        engine = markdown.Markdown(extensions=["fenced_code", "tables", "nl2br", "sane_lists"])
        if "html_block" in engine.preprocessors:
            engine.preprocessors.deregister("html_block")
        result = engine.convert(value)
        engine.reset()
        if result.count("<p>") == 1:
            result = result.replace("<p>", "", 1).rsplit("</p>", 1)[0]
    except ImportError:
        result = markdown_fallback(value)
    return sanitize_html(result)


def protect_regions(value):
    placeholders = []

    def protect(match):
        index = len(placeholders)
        placeholders.append(match.group(0))
        return f"\x00PROTECTED{index}\x00"

    value = re.sub(r"```[\s\S]*?```", protect, value or "")
    value = re.sub(r"`[^`\n]+`", protect, value)
    value = re.sub(r"\[[^\]]+\]\([^)]+\)", protect, value)
    return value, placeholders


def outbound_mentions(value):
    protected, _ = protect_regions(value)
    result = []
    seen = set()
    for match in MXID_IN_TEXT_RE.finditer(protected):
        mxid = match.group(1)
        if mxid not in seen:
            seen.add(mxid)
            result.append(mxid)
    return result


def inject_mention_links(value):
    protected, placeholders = protect_regions(value)
    protected = MXID_IN_TEXT_RE.sub(lambda match: f"[{match.group(1)}](https://matrix.to/#/{match.group(1)})", protected)
    for index, fragment in enumerate(placeholders):
        protected = protected.replace(f"\x00PROTECTED{index}\x00", fragment)
    return protected


def room_mention(value):
    protected, _ = protect_regions(value)
    return bool(re.search(r"(?<![\w/])@room(?![\w:.-])", protected))


def build_text_content(text, allow_room_mentions=False, notice=False, explicit_html=None):
    text = str(text or "")
    result = {"msgtype": "m.notice" if notice else "m.text", "body": text}
    users = outbound_mentions(text)
    mentions = {}
    if users:
        mentions["user_ids"] = users
    if allow_room_mentions and room_mention(text):
        mentions["room"] = True
    if mentions:
        result["m.mentions"] = mentions
    formatted = sanitize_html(explicit_html) if explicit_html is not None else markdown_to_html(inject_mention_links(text))
    if formatted and formatted != html.escape(text):
        result["format"] = "org.matrix.custom.html"
        result["formatted_body"] = formatted
    return result


def relation_content(thread_event_id=None, reply_to_event_id=None):
    if not thread_event_id and not reply_to_event_id:
        return None
    result = {}
    if reply_to_event_id:
        result["m.in_reply_to"] = {"event_id": reply_to_event_id}
    if thread_event_id:
        result["rel_type"] = "m.thread"
        result["event_id"] = thread_event_id
        result["is_falling_back"] = True
        result.setdefault("m.in_reply_to", {"event_id": reply_to_event_id or thread_event_id})
    return result


def apply_relation(content, thread_event_id=None, reply_to_event_id=None):
    relation = relation_content(thread_event_id, reply_to_event_id)
    if relation:
        content["m.relates_to"] = relation
    return content


def split_text(value, maximum):
    value = str(value or "")
    if len(value) <= maximum:
        return [value]
    result = []
    remaining = value
    fence_open = False
    fence_language = ""
    while remaining:
        if len(remaining) <= maximum:
            chunk = remaining
            remaining = ""
        else:
            boundary = max(1, maximum - (len(fence_language) + 8 if fence_open else 0))
            cut = remaining.rfind("\n\n", 0, boundary + 1)
            if cut < boundary // 2:
                cut = remaining.rfind("\n", 0, boundary + 1)
            if cut < boundary // 2:
                cut = remaining.rfind(" ", 0, boundary + 1)
            if cut <= 0:
                cut = boundary
            chunk = remaining[:cut].rstrip()
            remaining = remaining[cut:].lstrip()
        fences = list(re.finditer(r"(?m)^```([^\n]*)$", chunk))
        if fence_open:
            chunk = f"```{fence_language}\n" + chunk
        for match in fences:
            if fence_open:
                fence_open = False
                fence_language = ""
            else:
                fence_open = True
                fence_language = match.group(1).strip()
        if fence_open and remaining:
            chunk += "\n```"
        result.append(chunk)
    return result
