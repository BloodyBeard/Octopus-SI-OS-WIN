import re


SPECIALS = "_*[]()~`>#+-=|{}.!\\"
TABLE_SEPARATOR_RE = re.compile(
    r"^\s*\|?\s*:?-{3,}:?\s*(?:\|\s*:?-{3,}:?\s*)+\|?\s*$"
)
TASK_LIST_RE = re.compile(r"(?m)^\s*[-*]\s+\[[ xX]\]\s+")
DETAILS_RE = re.compile(r"<details\b[^>]*>.*?</details>", re.IGNORECASE | re.DOTALL)
DETAILS_TAG_RE = re.compile(r"(?m)^\s*</?(?:details|summary)\b")
MATH_RE = re.compile(
    r"(\$\$.*?\$\$|\\\[.*?\\\]|\\\(.*?\\\)|"
    r"\\(?:sum|frac|alpha|beta|gamma|delta|theta|lambda|mu|pi|sigma|"
    r"int|prod|sqrt|lim|infty|begin\{(?:equation|align|matrix|cases)\}))",
    re.IGNORECASE | re.DOTALL,
)
CJK_RE = re.compile(
    "["
    "\u3040-\u30ff"
    "\u3400-\u4dbf"
    "\u4e00-\u9fff"
    "\uac00-\ud7af"
    "\uf900-\ufaff"
    "\U00020000-\U000323af"
    "]"
)
FENCE_START_RE = re.compile(r"^\s*```([A-Za-z0-9_+.-]*)\s*$")
HEADING_RE = re.compile(r"^\s{0,3}#{1,6}\s+(.+?)\s*#*\s*$")
UNORDERED_RE = re.compile(r"^(\s*)[-+*]\s+(.+)$")
ORDERED_RE = re.compile(r"^(\s*)(\d+)[.)]\s+(.+)$")
QUOTE_RE = re.compile(r"^(\s*)>\s?(.*)$")


def text_units(text):
    return len(text.encode("utf-16-le")) // 2


def escape_plain(text):
    out = []
    for character in text:
        if character in SPECIALS:
            out.append("\\")
        out.append(character)
    return "".join(out)


def _escape_code(text):
    return text.replace("\\", "\\\\").replace("`", "\\`")


def _escape_url(text):
    return text.replace("\\", "\\\\").replace(")", "\\)")


def _find_unescaped(text, needle, start):
    cursor = start
    while True:
        cursor = text.find(needle, cursor)
        if cursor < 0:
            return -1
        slashes = 0
        probe = cursor - 1
        while probe >= 0 and text[probe] == "\\":
            slashes += 1
            probe -= 1
        if slashes % 2 == 0:
            return cursor
        cursor += len(needle)


def _parse_link(text, start, depth):
    label_end = _find_unescaped(text, "]", start + 1)
    if label_end < 0 or label_end + 1 >= len(text) or text[label_end + 1] != "(":
        return None
    cursor = label_end + 2
    nested = 0
    escaped = False
    while cursor < len(text):
        character = text[cursor]
        if escaped:
            escaped = False
        elif character == "\\":
            escaped = True
        elif character == "(":
            nested += 1
        elif character == ")":
            if nested == 0:
                label = _format_inline(text[start + 1 : label_end], depth + 1)
                destination = text[label_end + 2 : cursor].strip()
                if not destination:
                    return None
                return f"[{label}]({_escape_url(destination)})", cursor + 1
            nested -= 1
        cursor += 1
    return None


def _format_inline(text, depth=0):
    if depth > 24:
        return escape_plain(text)
    out = []
    cursor = 0
    markers = (("**", "*"), ("__", "__"), ("~~", "~"), ("||", "||"))
    while cursor < len(text):
        if text[cursor] == "\\" and cursor + 1 < len(text):
            out.append(escape_plain(text[cursor + 1]))
            cursor += 2
            continue
        if text[cursor] == "`":
            ticks = 1
            while cursor + ticks < len(text) and text[cursor + ticks] == "`":
                ticks += 1
            token = "`" * ticks
            end = _find_unescaped(text, token, cursor + ticks)
            if end >= 0:
                out.append(f"`{_escape_code(text[cursor + ticks:end])}`")
                cursor = end + ticks
                continue
        if text[cursor] == "[":
            parsed = _parse_link(text, cursor, depth)
            if parsed is not None:
                rendered, cursor = parsed
                out.append(rendered)
                continue
        matched = False
        for source, target in markers:
            if text.startswith(source, cursor):
                end = _find_unescaped(text, source, cursor + len(source))
                if end > cursor + len(source):
                    inner = _format_inline(text[cursor + len(source) : end], depth + 1)
                    out.append(f"{target}{inner}{target}")
                    cursor = end + len(source)
                    matched = True
                    break
        if matched:
            continue
        character = text[cursor]
        if character in ("*", "_"):
            end = _find_unescaped(text, character, cursor + 1)
            if end > cursor + 1:
                inner = _format_inline(text[cursor + 1 : end], depth + 1)
                out.append(f"_{inner}_")
                cursor = end + 1
                continue
        out.append(escape_plain(character))
        cursor += 1
    return "".join(out)


def _split_table_row(line):
    value = line.strip()
    if value.startswith("|"):
        value = value[1:]
    if value.endswith("|") and not value.endswith("\\|"):
        value = value[:-1]
    cells = []
    current = []
    escaped = False
    for character in value:
        if escaped:
            current.append(character)
            escaped = False
        elif character == "\\":
            escaped = True
        elif character == "|":
            cells.append("".join(current).strip())
            current = []
        else:
            current.append(character)
    if escaped:
        current.append("\\")
    cells.append("".join(current).strip())
    return cells


def _plain_cell(value):
    value = re.sub(r"!\[([^]]*)\]\([^)]+\)", r"\1", value)
    value = re.sub(r"\[([^]]+)\]\([^)]+\)", r"\1", value)
    value = re.sub(r"(`+|\*\*|__|~~|\|\|)", "", value)
    return value.strip()


def _render_table(lines):
    rows = [_split_table_row(line) for line in lines if not TABLE_SEPARATOR_RE.match(line)]
    if not rows:
        return ""
    width = max(len(row) for row in rows)
    normalized = [row + [""] * (width - len(row)) for row in rows]
    widths = [max(len(_plain_cell(row[index])) for row in normalized) for index in range(width)]
    rendered = []
    for row_index, row in enumerate(normalized):
        rendered.append(
            " | ".join(
                _plain_cell(value).ljust(widths[index]) for index, value in enumerate(row)
            ).rstrip()
        )
        if row_index == 0 and len(normalized) > 1:
            rendered.append("-+-".join("-" * max(1, value) for value in widths))
    return "\n".join(rendered)


def _legacy_blocks(text, pretty_tables):
    lines = text.replace("\r\n", "\n").replace("\r", "\n").split("\n")
    blocks = []
    index = 0
    while index < len(lines):
        fence = FENCE_START_RE.match(lines[index])
        if fence:
            language = fence.group(1)
            body = []
            start = index
            index += 1
            while index < len(lines) and not FENCE_START_RE.match(lines[index]):
                body.append(lines[index])
                index += 1
            if index < len(lines):
                index += 1
            raw = "\n".join(lines[start:index])
            rendered = f"```{escape_plain(language)}\n{_escape_code(chr(10).join(body))}\n```"
            blocks.append((raw, rendered))
            continue
        if index + 1 < len(lines) and "|" in lines[index] and TABLE_SEPARATOR_RE.match(lines[index + 1]):
            table = [lines[index], lines[index + 1]]
            index += 2
            while index < len(lines) and "|" in lines[index] and lines[index].strip():
                table.append(lines[index])
                index += 1
            raw = "\n".join(table)
            if pretty_tables:
                rendered = f"```\n{_escape_code(_render_table(table))}\n```"
            else:
                rendered = "\n".join(_format_inline(line) for line in table)
            blocks.append((raw, rendered))
            continue
        line = lines[index]
        heading = HEADING_RE.match(line)
        quote = QUOTE_RE.match(line)
        unordered = UNORDERED_RE.match(line)
        ordered = ORDERED_RE.match(line)
        if heading:
            rendered = f"*{_format_inline(heading.group(1))}*"
        elif quote:
            rendered = f"{escape_plain(quote.group(1))}>{_format_inline(quote.group(2))}"
        elif unordered:
            rendered = f"{unordered.group(1)}\\- {_format_inline(unordered.group(2))}"
        elif ordered:
            rendered = (
                f"{ordered.group(1)}{ordered.group(2)}\\. "
                f"{_format_inline(ordered.group(3))}"
            )
        else:
            rendered = _format_inline(line)
        blocks.append((line, rendered))
        index += 1
    return blocks


def _split_scalar(text, limit):
    chunks = []
    current = []
    units = 0
    for character in text:
        width = 2 if ord(character) > 0xFFFF else 1
        if current and units + width > limit:
            chunks.append("".join(current))
            current = []
            units = 0
        if width > limit:
            raise ValueError("telegram text limit cannot fit one Unicode scalar")
        current.append(character)
        units += width
    if current:
        chunks.append("".join(current))
    return chunks


def _split_raw_block(raw, limit, pretty_tables):
    words = re.split(r"(\s+)", raw)
    chunks = []
    current = ""
    for word in words:
        candidate = current + word
        rendered = "\n".join(value for _source, value in _legacy_blocks(candidate, pretty_tables))
        if current and text_units(rendered) > limit:
            chunks.append(current.rstrip())
            current = word.lstrip()
        else:
            current = candidate
        rendered_current = "\n".join(
            value for _source, value in _legacy_blocks(current, pretty_tables)
        )
        if text_units(rendered_current) > limit:
            fragments = _split_scalar(current, max(1, limit // 2))
            if len(fragments) == 1:
                raise ValueError("telegram rendered fragment exceeds the configured limit")
            chunks.extend(fragment for fragment in fragments[:-1] if fragment)
            current = fragments[-1]
    if current:
        chunks.append(current)
    return [chunk for chunk in chunks if chunk]


def legacy_chunks(text, limit, pretty_tables=True):
    if not isinstance(text, str):
        raise ValueError("telegram text must be a string")
    if not isinstance(limit, int) or isinstance(limit, bool) or limit < 1:
        raise ValueError("telegram text limit must be a positive integer")
    blocks = _legacy_blocks(text, pretty_tables)
    chunks = []
    raw_parts = []
    rendered_parts = []

    def flush():
        if not raw_parts:
            return
        chunks.append(("\n".join(raw_parts), "\n".join(rendered_parts)))
        raw_parts.clear()
        rendered_parts.clear()

    for raw, rendered in blocks:
        separator_units = 1 if rendered_parts else 0
        used = text_units("\n".join(rendered_parts))
        if text_units(rendered) + separator_units + used <= limit:
            raw_parts.append(raw)
            rendered_parts.append(rendered)
            continue
        flush()
        if text_units(rendered) <= limit:
            raw_parts.append(raw)
            rendered_parts.append(rendered)
            continue
        for fragment in _split_raw_block(raw, limit, pretty_tables):
            fragment_rendered = "\n".join(
                value for _source, value in _legacy_blocks(fragment, pretty_tables)
            )
            if text_units(fragment_rendered) > limit:
                for scalar in _split_scalar(fragment, max(1, limit // 2)):
                    scalar_rendered = escape_plain(scalar)
                    if text_units(scalar_rendered) > limit:
                        raise ValueError("telegram escaped scalar fragment exceeds the configured limit")
                    chunks.append((scalar, scalar_rendered))
            else:
                chunks.append((fragment, fragment_rendered))
    flush()
    return chunks or [("", "")]


def needs_rich_rendering(content):
    if not content:
        return False
    lines = content.splitlines()
    return bool(
        any(TABLE_SEPARATOR_RE.match(line) for line in lines)
        or TASK_LIST_RE.search(content)
        or DETAILS_TAG_RE.search(content)
        or "$$" in content
    )


def rich_safe(content, max_chars, reject_details_math=True, reject_cjk=False):
    if not content or not content.strip() or len(content) > max_chars:
        return False
    if reject_details_math:
        for block in DETAILS_RE.findall(content):
            if MATH_RE.search(block):
                return False
    if reject_cjk and CJK_RE.search(content):
        return False
    return True


def _rich_linebreaks(content):
    lines = content.replace("\r\n", "\n").replace("\r", "\n").split("\n")
    output = []
    fenced = False
    for line in lines:
        if FENCE_START_RE.match(line):
            fenced = not fenced
            output.append(line)
        elif fenced or not line:
            output.append(line)
        elif line.endswith("  "):
            output.append(line)
        else:
            output.append(f"{line}  ")
    return "\n".join(output).rstrip()


def rich_message_payload(content, skip_entity_detection=False):
    payload = {"markdown": _rich_linebreaks(content)}
    if skip_entity_detection:
        payload["skip_entity_detection"] = True
    return payload
