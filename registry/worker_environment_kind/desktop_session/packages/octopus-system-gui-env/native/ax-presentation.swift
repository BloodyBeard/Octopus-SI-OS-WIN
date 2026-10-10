import Foundation

struct DesktopAXPresentation: Codable, Equatable {
    let schema_version: UInt32
    let format: String
    let context_type: String
    let mode: String
    let base_revision: UInt64?
    let text: String
    let rendered_element_ids: [String]
    let focused_element_id: String?
    let selected_text: String?
    let selected_element_ids: [String]
}

struct DesktopAXPresentationProfile: Codable, Sendable, Equatable {
    let context_type: String
    let hide_cancel_actions: Bool
    let omit_calendar_event_children: Bool
    let excluded_action_descriptions: [String]
    let capture_note: String?
}

struct DesktopAXDisplayFacts {
    let id: String
    var role: String
    let subrole: String?
    var roleDescription: String?
    var title: String?
    var description: String?
    var value: String?
    let nativeTitle: String?
    let nativeBooleanValue: Bool?
    let valueDescription: String?
    let valueTypeName: String?
    let placeholder: String?
    let help: String?
    let identifier: String?
    var url: URL?
    let domClasses: [String]
    var actions: [DesktopAXAction]
    let selected: Bool
    let selectable: Bool
    let focused: Bool
    var focusable: Bool
    let enabled: Bool
    let disclosing: Bool
    let disclosable: Bool
    let valueSettable: Bool
    let horizontalScrollBar: Bool
    let verticalScrollBar: Bool
    var titleTargets: [String]
    let rowRange: Range<Int>?
    let columnRange: Range<Int>?
    let attributed: DesktopAXAttributedInput?
    var attributedSource: DesktopAXAttributedSource?
    var mapped: DesktopAXMappedText?
    var children: [Self]
    var complete: Bool

    var ownDescriptive: Bool {
        role == "AXImage" || [description, value, title, placeholder].contains { !($0 ?? "").isEmpty }
    }

    var descriptive: Bool { ownDescriptive || children.contains(where: \.descriptive) }
    var descendants: [Self] { children.flatMap { [$0] + $0.descendants } }
    var subtree: [Self] { [self] + descendants }
    var interestingActions: Bool { actions.contains { !["AXScrollToVisible", "AXShowMenu"].contains($0.name) } }
    var canBeUserInteractive: Bool {
        switch role {
        case "AXStaticText": return focusable || selectable
        case "AXTextArea", "AXGroup": return focusable
        default: return true
        }
    }
}

struct DesktopAXDisplayTree {
    let roots: [DesktopAXDisplayFacts]
    let contextType: String
    let profile: DesktopAXPresentationProfile

    static func transform(_ roots: [DesktopAXDisplayFacts], profile: DesktopAXPresentationProfile,
                          urls: inout DesktopAXURLRendering, check: () throws -> Void) throws -> Self {
        guard ["full_ui", "event_stream", "informational"].contains(profile.context_type),
              roots.reduce(0, { $0 + $1.subtree.count }) <= 4096 else {
            throw DesktopFailure(code: "invalid_ax_presentation", message: "Native display context or retained tree exceeds its supported domain")
        }
        var transformed: [DesktopAXDisplayFacts] = []
        var textBudget = DesktopAXMappedText.maximumBytes
        for root in roots {
            if let result = try transformNode(root, profile: profile, urls: &urls, textBudget: &textBudget, check: check) { transformed.append(result) }
        }
        return Self(roots: transformed, contextType: profile.context_type, profile: profile)
    }

    private static func transformNode(_ original: DesktopAXDisplayFacts, profile: DesktopAXPresentationProfile,
                                      urls: inout DesktopAXURLRendering, textBudget: inout Int, check: () throws -> Void) throws -> DesktopAXDisplayFacts? {
        try check()
        var node = original
        let actionFacts = node
        node.actions.removeAll { action in
            switch action.name {
            case "AXShowDefaultUI", "AXShowAlternateUI": return true
            case "AXCancel": return profile.hide_cancel_actions || ["AXMenuBar", "AXMenuItem"].contains(actionFacts.role)
            case "AXPick": return actionFacts.role == "AXMenuItem"
            case "AXScrollLeftByPage", "AXScrollRightByPage": return !actionFacts.horizontalScrollBar
            case "AXScrollUpByPage", "AXScrollDownByPage": return !actionFacts.verticalScrollBar
            case "AXConfirm": return actionFacts.role == "AXTextField"
            case "AXIncrement", "AXDecrement": return actionFacts.valueSettable
            default: return false
            }
        }
        try associateTitles(&node, check: check)
        let descendants = node.descendants
        if !node.focused, node.role != "AXTable", node.subrole != "AXTableRow",
           !descendants.contains(where: { $0.valueSettable || $0.selectable }) {
            if node.selectable {
                let all = node.subtree
                node.description = try flattened(all.map(\.description))
                node.value = try flattened(all.map(\.value))
                node.title = try flattened(all.map(\.title))
                node.focusable = all.contains(where: \.focusable)
                node.mapped = nil
                node.children = descendants.filter { $0.nativeBooleanValue != nil }
            }
        }
        if node.complete, !node.focusable, !node.selectable, !node.interestingActions, !node.descriptive,
           !node.descendants.contains(where: { $0.focusable || $0.selectable || $0.interestingActions }) { return nil }
        if node.complete, !node.enabled, !node.focused, node.children.isEmpty, !node.descriptive { return nil }
        if node.role == "AXGroup", !node.selectable, !node.focused, !node.ownDescriptive,
           node.children.count == 1, node.children[0].complete {
            return try transformNode(node.children[0], profile: profile, urls: &urls, textBudget: &textBudget, check: check)
        }
        if !node.selectable, !node.focused, !node.ownDescriptive {
            let parentRow = try row(node, profile: profile, urls: &urls)
            for index in node.children.indices.reversed() {
                let child = node.children[index]
                if child.complete, child.role == node.role, try row(child, profile: profile, urls: &urls) == parentRow {
                    node.children.replaceSubrange(index...index, with: child.children)
                }
            }
        }
        if !node.focusable, !node.selectable, !node.descendants.contains(where: \.canBeUserInteractive) {
            var changed = true
            while changed {
                try check(); changed = false
                for index in node.children.indices.reversed() {
                    let child = node.children[index]
                    let description = (child.description ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
                    let value = (child.value ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
                    if child.complete && ((!description.isEmpty && (node.description ?? "").contains(description)) ||
                        (!value.isEmpty && (node.value ?? "").contains(value))) {
                        node.children.replaceSubrange(index...index, with: child.children); changed = true
                    }
                }
            }
        }
        if node.role == "AXLink", let url = node.url, let value = urls.render(url), !value.isEmpty {
            node.role = "AXStaticText"; node.roleDescription = "link"; node.value = value
            node.mapped = try DesktopAXMappedText(value); node.url = nil; node.children = []
        }
        if !node.selectable, !node.focusable {
            var groups: [Range<Int>] = []
            var start: Int?
            for index in 0...node.children.count {
                let qualifies = index < node.children.count && text(node.children[index], context: profile.context_type, urls: &urls) != nil
                if qualifies { if start == nil { start = index } }
                else if let first = start { if index - first > 1 { groups.append(first..<index) }; start = nil }
            }
            for group in groups.reversed() {
                var retained = node.children[group.lowerBound]
                let parts = node.children[group].compactMap { text($0, context: profile.context_type, urls: &urls) }
                    .filter { !$0.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }
                let value = parts.joined(separator: " ")
                try bound(value)
                retained.role = "AXStaticText"; retained.roleDescription = "text"
                if retained.value != nil { retained.value = value } else { retained.description = value }
                retained.mapped = nil; retained.children = []
                node.children.replaceSubrange(group, with: [retained])
            }
        }
        if profile.omit_calendar_event_children, node.role == "AXStaticText", node.roleDescription == "Event",
           node.children.count == 1, node.children[0].role == "AXStaticText" { node.children = [] }
        if node.role == "AXTextArea", let attributed = node.attributed {
            let rendered = try attributed.render(urls: &urls, check: check)
            guard rendered.mapped.text.utf8.count <= textBudget else { throw DesktopFailure(code: "ax_presentation_limit", message: "Retained attributed display exceeds the observation text bound") }
            textBudget -= rendered.mapped.text.utf8.count
            node.attributedSource = rendered; node.mapped = rendered.mapped; node.value = rendered.mapped.text
        }
        var children: [DesktopAXDisplayFacts] = []
        for child in node.children {
            if let transformed = try transformNode(child, profile: profile, urls: &urls, textBudget: &textBudget, check: check) { children.append(transformed) }
        }
        node.children = children
        return node
    }

    private static func associateTitles(_ node: inout DesktopAXDisplayFacts, check: () throws -> Void) throws {
        let labels = node.children.filter { !$0.titleTargets.isEmpty }
        for label in labels {
            try check()
            guard let labelIndex = node.children.firstIndex(where: { $0.id == label.id }) else { continue }
            let targets = label.titleTargets.compactMap { id in node.children.firstIndex(where: { $0.id == id }) }
            guard targets.count == label.titleTargets.count, Set(targets).count == targets.count,
                  !targets.contains(labelIndex) else { continue }
            let values = [label.value, label.title, label.description].compactMap { $0 }
            if targets.count == 1, values.count == 1, node.children[targets[0]].nativeTitle == nil {
                node.children[targets[0]].title = values[0]
                node.children.remove(at: labelIndex)
            } else if label.children.isEmpty {
                var group = label
                group.children = targets.map { node.children[$0] }
                node.children[labelIndex] = group
                for index in targets.sorted(by: >) { node.children.remove(at: index) }
            }
        }
    }

    private static func flattened(_ values: [String?]) throws -> String? {
        let parts = values.compactMap { $0?.trimmingCharacters(in: .whitespacesAndNewlines) }.filter { !$0.isEmpty }
        guard !parts.isEmpty else { return nil }
        let value = parts.joined(separator: "\n"); try bound(value); return value
    }

    private static func text(_ node: DesktopAXDisplayFacts, context: String, urls: inout DesktopAXURLRendering) -> String? {
        if node.role == "AXStaticText" { return node.value }
        if context == "informational", node.role == "AXLink", let url = node.url { return urls.render(url) }
        return nil
    }

    static func row(_ node: DesktopAXDisplayFacts, profile: DesktopAXPresentationProfile,
                    urls: inout DesktopAXURLRendering) throws -> String {
        let role: String
        if let description = node.roleDescription {
            switch node.role {
            case "AXMenuBarItem", "AXMenuItem", "AXRadioGroup": role = ""
            case "AXRow": role = "row"
            case "AXGenericElement": role = description.isEmpty ? "element" : description
            case "AXGroup": role = "container"
            default: role = description
            }
        } else {
            switch node.role {
            case "AXMenuBar": role = "menu bar"
            case "AXMenuBarItem", "AXMenuItem", "AXRadioGroup": role = ""
            default: role = profile.context_type == "informational" ? "" : "unknown"
            }
        }
        var traits: [String] = []
        if node.selectable && !node.selected && profile.context_type == "full_ui" { traits.append("selectable") }
        if node.selected { traits.append("selected") }
        if !node.enabled && profile.context_type != "informational" && !["AXGroup", "AXStaticText"].contains(node.role) { traits.append("disabled") }
        if node.disclosable { traits.append(node.disclosing ? "expanded" : "collapsed") }
        if node.valueSettable, let type = node.valueTypeName { traits.append(type == "string" ? "settable" : "settable, " + type) }
        if node.domClasses.contains("is-sender") { traits.append("sent by me") }
        var fields: [(String?, String)] = []
        var seen = Set<String>()
        func add(_ label: String?, _ value: String?) throws {
            guard let value, !value.isEmpty, seen.insert(value).inserted else { return }
            try bound(value); fields.append((label, value))
        }
        try add(nil, node.title)
        try add("Description", node.description)
        if let range = node.rowRange, range.lowerBound >= 0, !range.isEmpty { try add("Rows", "\(range.lowerBound)-\(range.upperBound)") }
        if let range = node.columnRange, range.lowerBound >= 0, !range.isEmpty { try add("Columns", "\(range.lowerBound)-\(range.upperBound)") }
        if let url = node.url { try add("URL", urls.render(url, isImage: node.role == "AXImage")) }
        try add("Help", node.help)
        try add("Value", node.mapped?.text ?? node.value)
        try add("Details", node.valueDescription)
        try add("Placeholder", node.placeholder)
        if let id = node.identifier, !id.hasPrefix("_"), !id.hasPrefix("NS") { try add("ID", id) }
        let secondary = node.actions.filter { !["AXPress", "AXShowMenu", "AXScrollToVisible"].contains($0.name) }
            .compactMap(\.description).filter { !profile.excluded_action_descriptions.contains($0) }
        if !secondary.isEmpty { try add("Secondary Actions", secondary.joined(separator: ", ")) }
        let short = fields.filter { $0.1.count < 101 && !$0.1.contains("\n") }
        let long = fields.filter { $0.1.count >= 101 || $0.1.contains("\n") }
        let ordered = short + long
        let value: String
        if ordered.count == 1, ordered[0].0 != "Secondary Actions" { value = ordered[0].1 }
        else { value = ordered.map { label, value in label.map { $0 + ": " + value } ?? value }.joined(separator: ", ") }
        let result = [role, traits.isEmpty ? "" : "(" + traits.joined(separator: ", ") + ")", value].filter { !$0.isEmpty }.joined(separator: " ")
        try bound(result); return result
    }

    static func bound(_ value: String) throws {
        guard value.utf8.count <= DesktopAXMappedText.maximumBytes else {
            throw DesktopFailure(code: "ax_presentation_limit", message: "Native display text exceeds the owned observation bound")
        }
    }
}

struct DesktopAXPresentationBase: Codable, Equatable {
    let app: DesktopAppRef
    let observation_id: String
    let revision: UInt64
}

struct DesktopAXRenderedNode {
    let id: String
    let text: String
    let children: [Self]
    var count: Int { 1 + children.reduce(0) { $0 + $1.count } }
    var ids: [String] { [id] + children.flatMap(\.ids) }

    func lines(depth: Int, prefix: String = "") -> [String] {
        [prefix + String(repeating: "\t", count: depth) + id + " " + text] + children.flatMap { $0.lines(depth: depth + 1, prefix: prefix) }
    }
}

struct DesktopAXRenderedTree {
    let roots: [DesktopAXRenderedNode]
    let facts: [DesktopAXDisplayFacts]
    let windowTitle: String?
    let applicationName: String
    let selectedText: String?
    let profile: DesktopAXPresentationProfile

    static func render(_ roots: [DesktopAXDisplayFacts], windowTitle: String?, applicationName: String,
                       selectedText: String?, profile: DesktopAXPresentationProfile, check: () throws -> Void) throws -> Self {
        var urls = DesktopAXURLRendering()
        while true {
            try check(); urls.reset()
            let tree = try DesktopAXDisplayTree.transform(roots, profile: profile, urls: &urls, check: check)
            var rowBytes = 0
            func renderNode(_ node: DesktopAXDisplayFacts) throws -> DesktopAXRenderedNode {
                try check()
                let text = try DesktopAXDisplayTree.row(node, profile: profile, urls: &urls)
                guard text.utf8.count <= DesktopAXMappedText.maximumBytes - rowBytes else { throw DesktopFailure(code: "ax_presentation_limit", message: "Rendered tree exceeds the observation text bound") }
                rowBytes += text.utf8.count
                return try DesktopAXRenderedNode(id: node.id, text: text, children: node.children.map(renderNode))
            }
            let rendered = try tree.roots.map(renderNode)
            if urls.total <= 4000 || !urls.compact() {
                let ids = rendered.flatMap(\.ids)
                guard Set(ids).count == ids.count else { throw DesktopFailure(code: "ax_presentation_identity", message: "Native display transformation repeated a retained element") }
                return Self(roots: rendered, facts: tree.roots.flatMap(\.subtree), windowTitle: windowTitle,
                            applicationName: applicationName, selectedText: selectedText, profile: profile)
            }
        }
    }

    func presentation(base: Self?, baseRevision: UInt64?) throws -> DesktopAXPresentation {
        let mode: String
        var text: String
        var revision: UInt64?
        if let base, let baseRevision, let changes = try difference(from: base) {
            revision = baseRevision
            if changes.isEmpty {
                mode = "no_change"
                text = "There has been no change in the accessibility tree for " + applicationName + "."
            } else {
                mode = "diff"
                text = "The following is a diff from the previous accessibility tree for " + applicationName + " with ~, +, and - representing changed, added, and removed elements, respectively.\n" + changes.joined(separator: "\n")
            }
        } else {
            mode = "full"; revision = nil
            let header = "Window: \"" + (windowTitle ?? "Unknown") + "\", App: " + applicationName + ".\n"
            let fullLines: [String] = roots.flatMap { $0.lines(depth: 0) }
            text = header + fullLines.joined(separator: "\n") + "\n"
        }
        let ids = roots.flatMap(\.ids)
        let selected = facts.filter(\.selected).map(\.id)
        let focused = facts.first(where: \.focused)?.id
        let rows = Dictionary(uniqueKeysWithValues: flattened(roots).map { ($0.id, $0.text) })
        if let selectedText, !selectedText.isEmpty {
            text += "\nSelected text: ```\n" + selectedText + "\n```"
        } else if !selected.isEmpty {
            text += "\nSelected:\n" + selected.compactMap { id in rows[id].map { id + " " + $0 } }.joined(separator: "\n")
        } else if let focused, let row = rows[focused] {
            text += "\nThe focused UI element is " + focused + " " + row
        }
        if (selectedText?.isEmpty == false) || !selected.isEmpty {
            text += "\n\nNote: Pay special attention to the content selected by the user. If the user asks a question or refers to the content they are looking at on-screen, they might be referring to the selected content (but they might be referring to something else that's visible, too)."
        }
        text += profile.capture_note ?? ""
        try DesktopAXDisplayTree.bound(text)
        return DesktopAXPresentation(schema_version: 1, format: "macos_ax", context_type: profile.context_type,
            mode: mode, base_revision: revision, text: text, rendered_element_ids: ids, focused_element_id: focused,
            selected_text: selectedText, selected_element_ids: selected)
    }

    private func flattened(_ roots: [DesktopAXRenderedNode]) -> [DesktopAXRenderedNode] {
        roots.flatMap { [$0] + flattened($0.children) }
    }

    private struct Change {
        let path: [Int]
        let priority: Int
        let node: DesktopAXRenderedNode
        var cost: Int { priority == 0 ? 0 : priority == 3 ? 1 : node.count }
        var lines: [String] {
            let depth = max(0, path.count - 1)
            switch priority {
            case 1: return node.lines(depth: depth, prefix: "-")
            case 2: return node.lines(depth: depth, prefix: "+")
            case 3: return ["~" + String(repeating: "\t", count: depth) + node.id + " " + node.text]
            default: return []
            }
        }
    }

    private func difference(from old: Self) throws -> [String]? {
        var queue: [([Int], [DesktopAXRenderedNode], [DesktopAXRenderedNode])] = [([], old.roots, roots)]
        var index = 0
        var changes: [Change] = []
        while index < queue.count {
            let (parent, before, after) = queue[index]; index += 1
            var prior = Dictionary(uniqueKeysWithValues: before.enumerated().map { ($0.element.id, ($0.offset, $0.element)) })
            let positions = after.compactMap { node in prior[node.id].map { (node.id, $0.0) } }
            let retained = Self.orderedSubset(positions)
            for (offset, node) in after.enumerated() {
                if let (_, previous) = prior.removeValue(forKey: node.id), retained.contains(node.id) {
                    changes.append(Change(path: parent + [offset], priority: previous.text == node.text ? 0 : 3, node: node))
                    queue.append((parent + [offset], previous.children, node.children))
                } else { changes.append(Change(path: parent + [offset], priority: 2, node: node)) }
            }
            for (_, item) in prior { changes.append(Change(path: parent + [item.0], priority: 1, node: item.1)) }
        }
        let fullCount = roots.reduce(0) { $0 + $1.count }
        var cost = 0
        for change in changes {
            cost += change.cost
            if cost > fullCount { return nil }
        }
        changes.sort { left, right in
            left.path == right.path ? left.priority < right.priority : left.path.lexicographicallyPrecedes(right.path)
        }
        let lines = changes.flatMap(\.lines)
        try DesktopAXDisplayTree.bound(lines.joined(separator: "\n"))
        return lines
    }

    private static func orderedSubset(_ positions: [(String, Int)]) -> Set<String> {
        var tails: [Int] = []
        var lengths = [Int](repeating: 0, count: positions.count)
        for index in positions.indices.reversed() {
            let value = -positions[index].1
            var lower = 0; var upper = tails.count
            while lower < upper {
                let middle = lower + (upper - lower) / 2
                if tails[middle] < value { lower = middle + 1 } else { upper = middle }
            }
            lengths[index] = lower + 1
            if lower == tails.count { tails.append(value) } else { tails[lower] = value }
        }
        var remaining = tails.count; var last = -1; var result = Set<String>()
        for (index, item) in positions.enumerated() where remaining > 0 && item.1 > last && lengths[index] >= remaining {
            result.insert(item.0); last = item.1; remaining -= 1
        }
        return result
    }
}
