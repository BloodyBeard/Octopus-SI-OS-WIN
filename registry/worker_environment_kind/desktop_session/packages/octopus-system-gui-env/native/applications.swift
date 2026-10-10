import AppKit
import ApplicationServices
import CoreServices
import CryptoKit
import Foundation

struct DesktopAppScope: Codable, Equatable {
    let host_generation: String
    let worker_environment_id: String
    let formation_pin: String

    func validate() throws {
        guard [host_generation, worker_environment_id, formation_pin].allSatisfy({ !$0.isEmpty && !$0.contains("\0") }) else {
            throw DesktopFailure(code: "invalid_app_scope", message: "Application scope requires exact host, room and formation identity")
        }
    }
}

struct DesktopAppSelector: Codable {
    enum Kind: String, Codable { case bundle_id, bundle_path }
    let kind: Kind
    let value: String
}

struct DesktopAppInfo: Codable {
    let app: DesktopAppRef
    let bundle_id: String
    let bundle_path: String
    let display_name: String
    let process_id: Int32
    let launch_time_unix_ms: UInt64?
    let active: Bool
    let hidden: Bool
}

struct DesktopAppResolution: Codable {
    let candidates: [DesktopAppInfo]
    let selected: DesktopAppRef?
}

struct DesktopApplicationInstance: Codable, Equatable {
    let app: DesktopAppRef
    let process_id: Int32
    let launch_time_unix_ms: UInt64?

    enum CodingKeys: String, CodingKey { case app, process_id, launch_time_unix_ms }
    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(app, forKey: .app)
        try container.encode(process_id, forKey: .process_id)
        try container.encode(launch_time_unix_ms, forKey: .launch_time_unix_ms)
    }
}

struct DesktopApplicationTarget: Codable, Equatable {
    let bundle_id: String
    let bundle_path: String
    let display_name: String
    let instance: DesktopApplicationInstance?

    enum CodingKeys: String, CodingKey { case bundle_id, bundle_path, display_name, instance }
    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(bundle_id, forKey: .bundle_id)
        try container.encode(bundle_path, forKey: .bundle_path)
        try container.encode(display_name, forKey: .display_name)
        try container.encode(instance, forKey: .instance)
    }
}


struct DesktopApplicationPolicy: Decodable {
    enum Access: String, Decodable { case allow, deny }
    struct AccessPolicy: Decodable {
        let default_access: Access
        let bundle_ids: [String: Access]
        func allows(_ identifier: String) -> Bool { (bundle_ids[identifier] ?? default_access) == .allow }
    }
    let schema_version: Int
    let enabled: Bool
    let allow_persistent_approval: Bool
    let required_access: AccessPolicy
    let configured_access: AccessPolicy
    let forbidden_bundle_ids: [String]
    let elevated_risk_bundle_ids: [String]
    let high_risk_url_schemes: [String]
    let high_risk_warning_subtitle: String

    static func read() throws -> (Self, String)? {
        guard let text = ProcessInfo.processInfo.environment["OCTOPUS_DESKTOP_APPLICATION_POLICY"] else { return nil }
        let bytes = Data(text.utf8)
        guard bytes.count <= 65536, let object = try JSONSerialization.jsonObject(with: bytes) as? [String: Any],
              Set(object.keys) == ["schema_version","enabled","allow_persistent_approval","required_access","configured_access",
                "forbidden_bundle_ids","elevated_risk_bundle_ids","high_risk_url_schemes","high_risk_warning_subtitle"] else {
            throw DesktopFailure(code: "application_policy_invalid", message: "Native eligibility requires its exact typed private policy snapshot")
        }
        for key in ["required_access", "configured_access"] {
            guard let access = object[key] as? [String: Any], Set(access.keys) == ["default_access", "bundle_ids"] else {
                throw DesktopFailure(code: "application_policy_invalid", message: "Native eligibility access policy has unexpected fields")
            }
        }
        let policy = try JSONDecoder().decode(Self.self, from: bytes)
        func exact(_ value: String) -> Bool {
            !value.isEmpty && value.utf8.count <= 4096 && !value.contains("\0") && value.trimmingCharacters(in: .whitespacesAndNewlines) == value
        }
        guard policy.schema_version == 1, exact(policy.high_risk_warning_subtitle),
              policy.required_access.bundle_ids.count <= 4096, policy.configured_access.bundle_ids.count <= 4096,
              policy.required_access.bundle_ids.keys.allSatisfy(exact), policy.configured_access.bundle_ids.keys.allSatisfy(exact) else {
            throw DesktopFailure(code: "application_policy_invalid", message: "Native eligibility policy contains invalid declared values")
        }
        for values in [policy.forbidden_bundle_ids, policy.elevated_risk_bundle_ids, policy.high_risk_url_schemes] {
            guard values.count <= 4096, values.allSatisfy(exact), Set(values).count == values.count else {
                throw DesktopFailure(code: "application_policy_invalid", message: "Native eligibility values must be bounded exact distinct identities")
            }
        }
        return (policy, SHA256.hash(data: bytes).map { String(format: "%02x", $0) }.joined())
    }

    func resolve(_ target: DesktopApplicationTarget, bundle: Bundle) -> DesktopNativeAppPolicy {
        let schemes = (bundle.object(forInfoDictionaryKey: "CFBundleURLTypes") as? [[String: Any]] ?? [])
            .flatMap { $0["CFBundleURLSchemes"] as? [String] ?? [] }
        let high = elevated_risk_bundle_ids.contains(target.bundle_id) || schemes.contains(where: high_risk_url_schemes.contains)
        let allowed = enabled && required_access.allows(target.bundle_id) && configured_access.allows(target.bundle_id)
        return DesktopNativeAppPolicy(decision: !allowed ? .denied : forbidden_bundle_ids.contains(target.bundle_id) ? .forbidden : .allowed,
            allow_persistent_approval: allow_persistent_approval, risk: high ? .high : .low,
            warning_subtitle: high ? high_risk_warning_subtitle : nil,
            target: .init(bundle_id: target.bundle_id, bundle_path: target.bundle_path, display_name: target.display_name))
    }
}

struct DesktopNativeAppPolicy: Codable, Equatable {
    enum Decision: String, Codable { case allowed, denied, forbidden }
    enum Risk: String, Codable { case high, low }
    struct Target: Codable, Equatable { let bundle_id: String; let bundle_path: String; let display_name: String }
    let decision: Decision
    let allow_persistent_approval: Bool
    let risk: Risk
    let warning_subtitle: String?
    let target: Target
    enum CodingKeys: String, CodingKey { case decision, allow_persistent_approval, risk, warning_subtitle, target }
    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(decision, forKey: .decision)
        try container.encode(allow_persistent_approval, forKey: .allow_persistent_approval)
        try container.encode(risk, forKey: .risk)
        try container.encode(warning_subtitle, forKey: .warning_subtitle)
        try container.encode(target, forKey: .target)
    }
    func requireAllowed() throws {
        guard decision == .allowed else {
            throw DesktopFailure(code: decision == .denied ? "application_policy_denied" : "application_policy_forbidden",
                message: "The declared native eligibility policy does not permit this application target")
        }
    }
}

struct DesktopPreparedApplication: Codable {
    let target_handle: String
    let application: DesktopApplicationTarget
    let native_policy: DesktopNativeAppPolicy?
    let native_policy_digest: String?
}

struct DesktopApplicationCatalogEntry: Codable {
    let application: DesktopApplicationTarget
    let active: Bool
    let last_used_unix_ms: UInt64?
    let use_count: UInt64?
}

@MainActor
private final class DesktopApplicationCatalog {
    struct Item {
        let bundleID: String
        let path: String
        let displayName: String
        let lastUsed: Date?
        let useCount: UInt64?
    }

    private var query: MDQuery?
    private var items: [String: Item] = [:]
    private var complete = false
    private var closed = false
    private var failure: Error?
    private var waiters: [CheckedContinuation<Void, Error>] = []

    func snapshot() async throws -> [Item] {
        try Task.checkCancellation()
        guard !closed else { throw DesktopFailure(code: "app_catalog_closed", message: "Application catalog was released") }
        if failure != nil, let query, MDQueryIsGatheringComplete(query) { update(finished: true, info: nil) }
        if let failure { throw failure }
        if query == nil { try start() }
        if !complete {
            guard waiters.count < 64 else { throw DesktopFailure(code: "app_catalog_busy", message: "Application catalog waiters exceed the native request bound") }
            try await withCheckedThrowingContinuation { waiters.append($0) }
        }
        try Task.checkCancellation()
        guard !closed else { throw DesktopFailure(code: "app_catalog_closed", message: "Application catalog was released during enumeration") }
        if let failure { throw failure }
        return Array(items.values)
    }

    private func start() throws {
        let expression = "kMDItemContentType == \"com.apple.application-bundle\" && kMDItemFSName == \"*.app\" && kMDItemLastUsedDate_Ranking >= $time.today(-14)"
        guard let query = MDQueryCreate(kCFAllocatorDefault, expression as CFString, nil, nil) else {
            throw DesktopFailure(code: "app_catalog_unavailable", message: "Native application metadata query could not be created")
        }
        self.query = query
        MDQuerySetDispatchQueue(query, DispatchQueue.main)
        MDQuerySetSearchScope(query, [kMDQueryScopeComputer] as CFArray, 0)
        let observer = Unmanaged.passUnretained(self).toOpaque()
        let object = Unmanaged.passUnretained(query).toOpaque()
        let names: [CFString] = [kMDQueryDidFinishNotification, kMDQueryDidUpdateNotification]
        for name in names {
            CFNotificationCenterAddObserver(CFNotificationCenterGetLocalCenter(), observer, Self.notification, name, object, .deliverImmediately)
        }
        guard MDQueryExecute(query, CFOptionFlags(kMDQueryWantsUpdates.rawValue)) else {
            close()
            throw DesktopFailure(code: "app_catalog_unavailable", message: "Native application metadata query did not start")
        }
    }

    private static let notification: CFNotificationCallback = { _, pointer, name, _, info in
        guard let pointer, let name else { return }
        MainActor.assumeIsolated {
            let catalog = Unmanaged<DesktopApplicationCatalog>.fromOpaque(pointer).takeUnretainedValue()
            catalog.update(finished: CFEqual(name.rawValue, kMDQueryDidFinishNotification), info: info)
        }
    }

    private func update(finished: Bool, info: CFDictionary?) {
        guard !closed, let query else { return }
        let rebuild = finished || failure != nil
        guard !rebuild || MDQueryIsGatheringComplete(query) else { return }
        MDQueryDisableUpdates(query)
        do {
            if rebuild {
                let count = MDQueryGetResultCount(query)
                guard count <= 4096 else { throw DesktopFailure(code: "app_catalog_limit", message: "Native application metadata inventory exceeds its bound") }
                var next: [String: Item] = [:]
                for index in 0..<count {
                    guard let raw = MDQueryGetResultAtIndex(query, index) else {
                        throw DesktopFailure(code: "app_catalog_invalid", message: "Native metadata query returned no item for an enumerated result")
                    }
                    let value = Unmanaged<AnyObject>.fromOpaque(raw).takeUnretainedValue()
                    if let item = try Self.item(value) { next[item.path] = item }
                }
                items = next
                complete = true
            } else if let info {
                let values = info as NSDictionary
                let keys: [CFString] = [kMDQueryUpdateRemovedItems, kMDQueryUpdateChangedItems, kMDQueryUpdateAddedItems]
                for key in keys {
                    guard let value = values[key] else { continue }
                    guard let entries = value as? [AnyObject], entries.count <= 4096 else {
                        throw DesktopFailure(code: "app_catalog_invalid", message: "Native metadata update is not a bounded item collection")
                    }
                    for entry in entries {
                        guard CFGetTypeID(entry) == MDItemGetTypeID() else {
                            throw DesktopFailure(code: "app_catalog_invalid", message: "Native metadata update has an unexpected item type")
                        }
                        let native = unsafeBitCast(entry, to: MDItem.self)
                        if let path = MDItemCopyAttribute(native, kMDItemPath) as? String {
                            items.removeValue(forKey: URL(fileURLWithPath: path).standardizedFileURL.resolvingSymlinksInPath().path)
                        }
                        if !CFEqual(key, kMDQueryUpdateRemovedItems), let item = try Self.item(entry) { items[item.path] = item }
                    }
                }
            }
            guard items.count <= 1024 else { throw DesktopFailure(code: "app_catalog_limit", message: "Native application catalog exceeds its declared result bound") }
            failure = nil
        } catch { failure = error }
        MDQueryEnableUpdates(query)
        if complete || failure != nil {
            let pending = waiters
            waiters.removeAll()
            for waiter in pending {
                if let failure { waiter.resume(throwing: failure) }
                else { waiter.resume() }
            }
        }
    }

    private static func item(_ value: AnyObject) throws -> Item? {
        guard CFGetTypeID(value) == MDItemGetTypeID() else {
            throw DesktopFailure(code: "app_catalog_invalid", message: "Native application query returned an unexpected result type")
        }
        let item = unsafeBitCast(value, to: MDItem.self)
        let keys: [CFString] = [kMDItemPath, kMDItemDisplayName, kMDItemCFBundleIdentifier,
                    "kMDItemLastUsedDate_Ranking" as CFString, "kMDItemUseCount" as CFString]
        guard let attributes = MDItemCopyAttributes(item, keys as CFArray) as? [String: Any],
              let path = attributes[kMDItemPath as String] as? String, path.hasPrefix("/"),
              let name = attributes[kMDItemDisplayName as String] as? String,
              let identifier = attributes[kMDItemCFBundleIdentifier as String] as? String, !identifier.isEmpty else {
            throw DesktopFailure(code: "app_catalog_metadata_unavailable", message: "An enumerated application has no readable path, display name or bundle identifier")
        }
        let url = URL(fileURLWithPath: path).standardizedFileURL.resolvingSymlinksInPath()
        guard url.pathExtension.lowercased() == "app", let bundle = Bundle(url: url), bundle.infoDictionary != nil else {
            throw DesktopFailure(code: "app_catalog_metadata_unavailable", message: "Application bundle metadata is unavailable at \(url.path)")
        }
        guard bundle.bundleIdentifier == identifier else {
            throw DesktopFailure(code: "app_catalog_identity_changed", message: "Application bundle identity differs from its metadata query result at \(url.path)")
        }
        guard (bundle.object(forInfoDictionaryKey: "LSUIElement") as? Bool) != true else { return nil }
        let hidden: Bool?
        do { hidden = try url.resourceValues(forKeys: [.isHiddenKey]).isHidden }
        catch { throw DesktopFailure(code: "app_catalog_metadata_unavailable", message: "Application visibility metadata is unavailable at \(url.path): \(error)") }
        guard let hidden else {
            throw DesktopFailure(code: "app_catalog_metadata_unavailable", message: "Application visibility metadata was not reported at \(url.path)")
        }
        guard !hidden else { return nil }
        let date = attributes["kMDItemLastUsedDate_Ranking"] as? Date
        let count = (attributes["kMDItemUseCount"] as? NSNumber).flatMap { value -> UInt64? in
            let number = value.doubleValue
            return number.isFinite && number >= 0 && number <= 9_007_199_254_740_991 && number.rounded(.towardZero) == number ? UInt64(number) : nil
        }
        return Item(bundleID: identifier, path: url.path, displayName: name.hasSuffix(".app") ? String(name.dropLast(4)) : name,
                    lastUsed: date, useCount: count)
    }

    func close() {
        guard !closed else { return }
        closed = true
        if let query {
            MDQueryStop(query)
            CFNotificationCenterRemoveEveryObserver(CFNotificationCenterGetLocalCenter(), Unmanaged.passUnretained(self).toOpaque())
        }
        query = nil
        items.removeAll()
        let pending = waiters
        waiters.removeAll()
        for waiter in pending { waiter.resume(throwing: DesktopFailure(code: "app_catalog_closed", message: "Native application catalog was released")) }
    }
}

@MainActor
final class DesktopApplications {
    let scope: DesktopAppScope
    private struct Entry {
        let reference: DesktopAppRef
        let application: NSRunningApplication
        let bundleID: String
        let bundlePath: String
        let processID: pid_t
    }
    private var entries: [String: Entry] = [:]
    private var targets: [String: (URL, DesktopApplicationTarget, DesktopNativeAppPolicy?)] = [:]
    private let policy: (DesktopApplicationPolicy, String)?
    private var catalog: DesktopApplicationCatalog?
    private var closed = false

    init(scope: DesktopAppScope) throws {
        try scope.validate()
        self.scope = scope
        policy = try DesktopApplicationPolicy.read()
    }

    func catalogEntries(check: @escaping () throws -> Void) async throws -> [DesktopApplicationCatalogEntry] {
        try requireOpen(); try check()
        if catalog == nil { catalog = DesktopApplicationCatalog() }
        guard let catalog else { throw DesktopFailure(code: "app_catalog_unavailable", message: "Native application catalog is unavailable") }
        let indexed = try await catalog.snapshot()
        try requireOpen(); try check()
        let usage = Dictionary(uniqueKeysWithValues: indexed.map { ($0.path, $0) })
        let cutoff = Date().addingTimeInterval(-14 * 86400)
        var result: [String: DesktopApplicationCatalogEntry] = [:]
        for item in indexed where item.lastUsed.map({ $0 >= cutoff }) == true {
            let target = DesktopApplicationTarget(bundle_id: item.bundleID, bundle_path: item.path, display_name: item.displayName, instance: nil)
            result[item.path] = DesktopApplicationCatalogEntry(application: target, active: false,
                last_used_unix_ms: Self.timestamp(item.lastUsed), use_count: item.useCount)
        }
        let running = try list()
        for app in running {
            let native = try application(app.app)
            guard native.activationPolicy == .regular, native.processIdentifier != getpid() else { continue }
            let indexed = usage[app.bundle_path]
            let target = DesktopApplicationTarget(bundle_id: app.bundle_id, bundle_path: app.bundle_path,
                display_name: app.display_name.isEmpty ? app.bundle_id : app.display_name,
                instance: DesktopApplicationInstance(app: app.app, process_id: app.process_id, launch_time_unix_ms: app.launch_time_unix_ms))
            result[app.bundle_path] = DesktopApplicationCatalogEntry(application: target, active: app.active,
                last_used_unix_ms: Self.timestamp(indexed?.lastUsed), use_count: indexed?.useCount)
        }
        guard result.count <= 1024 else { throw DesktopFailure(code: "app_catalog_limit", message: "Merged application inventory exceeds its result bound") }
        let ordered = result.values.sorted { left, right in
            if left.active != right.active { return left.active }
            if (left.application.instance != nil) != (right.application.instance != nil) { return left.application.instance != nil }
            let leftDate = usage[left.application.bundle_path]?.lastUsed ?? .distantPast
            let rightDate = usage[right.application.bundle_path]?.lastUsed ?? .distantPast
            if leftDate != rightDate { return leftDate > rightDate }
            if (left.use_count ?? 0) != (right.use_count ?? 0) { return (left.use_count ?? 0) > (right.use_count ?? 0) }
            let names = left.application.display_name.localizedCaseInsensitiveCompare(right.application.display_name)
            return names == .orderedSame ? left.application.bundle_path < right.application.bundle_path : names == .orderedAscending
        }
        try requireOpen(); try check()
        return ordered
    }

    private static func timestamp(_ date: Date?) -> UInt64? {
        date.map { $0.timeIntervalSince1970 * 1000 }.flatMap { $0.isFinite && $0 >= 1 && $0 <= 9_007_199_254_740_991 ? UInt64($0) : nil }
    }

    func list() throws -> [DesktopAppInfo] {
        try requireOpen()
        retireExited()
        var result: [DesktopAppInfo] = []
        for application in NSWorkspace.shared.runningApplications {
            guard !application.isTerminated, application.processIdentifier > 0,
                  let bundleID = application.bundleIdentifier, !bundleID.isEmpty,
                  let url = application.bundleURL, url.isFileURL else { continue }
            let path = url.standardizedFileURL.resolvingSymlinksInPath().path
            let entry: Entry
            if let existing = entries.values.first(where: { $0.application.isEqual(application) }) {
                guard existing.processID == application.processIdentifier,
                      existing.bundleID == bundleID, existing.bundlePath == path else {
                    entries.removeValue(forKey: existing.reference.app_instance_id)
                    continue
                }
                entry = existing
            } else {
                guard entries.count < 1024 else {
                    throw DesktopFailure(code: "app_inventory_limit", message: "Running application inventory exceeds its bound")
                }
                let reference = DesktopAppRef(host_generation: scope.host_generation, app_instance_id: UUID().uuidString.lowercased())
                entry = Entry(reference: reference, application: application, bundleID: bundleID, bundlePath: path, processID: application.processIdentifier)
                entries[reference.app_instance_id] = entry
            }
            result.append(info(entry))
        }
        return result.sorted { ($0.bundle_id, $0.bundle_path, $0.process_id) < ($1.bundle_id, $1.bundle_path, $1.process_id) }
    }

    func resolve(_ selector: DesktopAppSelector) throws -> DesktopAppResolution {
        guard !selector.value.isEmpty, selector.value.trimmingCharacters(in: .whitespacesAndNewlines) == selector.value,
              !selector.value.contains("\0") else {
            throw DesktopFailure(code: "invalid_app_selector", message: "Application selector must be a nonempty exact identity")
        }
        let candidates: [DesktopAppInfo]
        switch selector.kind {
        case .bundle_id:
            candidates = try list().filter { $0.bundle_id == selector.value }
        case .bundle_path:
            let url = URL(fileURLWithPath: selector.value)
            guard selector.value.hasPrefix("/"), url.standardizedFileURL.resolvingSymlinksInPath().path == selector.value else {
                throw DesktopFailure(code: "invalid_app_selector", message: "Application path must be absolute and canonical")
            }
            candidates = try list().filter { $0.bundle_path == selector.value }
        }
        return DesktopAppResolution(candidates: candidates, selected: candidates.count == 1 ? candidates[0].app : nil)
    }

    func application(_ reference: DesktopAppRef) throws -> NSRunningApplication {
        try requireOpen()
        guard reference.host_generation == scope.host_generation,
              let entry = entries[reference.app_instance_id], entry.reference == reference,
              !entry.application.isTerminated,
              let current = NSRunningApplication(processIdentifier: entry.processID),
              current.isEqual(entry.application), current.processIdentifier == entry.processID,
              current.bundleIdentifier == entry.bundleID,
              current.bundleURL?.standardizedFileURL.resolvingSymlinksInPath().path == entry.bundlePath else {
            entries.removeValue(forKey: reference.app_instance_id)
            throw DesktopFailure(code: "app_instance_unavailable", message: "The exact native application instance is no longer available")
        }
        return current
    }

    func prepareTarget(_ selector: DesktopAppSelector) throws -> DesktopPreparedApplication {
        try requireOpen()
        guard !selector.value.isEmpty, selector.value.count <= 4096, !selector.value.contains("\0"),
              selector.value.trimmingCharacters(in: .whitespacesAndNewlines) == selector.value else {
            throw DesktopFailure(code: "invalid_app_selector", message: "Application selector requires an exact bundle identity")
        }
        let running = try list()
        let urls: [URL]
        switch selector.kind {
        case .bundle_path:
            let url = URL(fileURLWithPath: selector.value).standardizedFileURL.resolvingSymlinksInPath()
            guard selector.value.hasPrefix("/"), url.path == selector.value else {
                throw DesktopFailure(code: "invalid_app_selector", message: "Application path must be absolute and canonical")
            }
            urls = [url]
        case .bundle_id:
            let installed = NSWorkspace.shared.urlsForApplications(withBundleIdentifier: selector.value)
            let current = running.filter { $0.bundle_id == selector.value }.map { URL(fileURLWithPath: $0.bundle_path) }
            urls = Array(Set((installed + current).map { $0.standardizedFileURL.resolvingSymlinksInPath() }))
        }
        guard urls.count == 1, let url = urls.first else {
            throw DesktopFailure(code: urls.isEmpty ? "application_not_found" : "application_target_ambiguous",
                                 message: "Select one exact installed bundle path; candidates: \(urls.map(\.path).sorted().joined(separator: ", "))")
        }
        guard url.isFileURL, url.pathExtension == "app", let bundle = Bundle(url: url),
              let identifier = bundle.bundleIdentifier, !identifier.isEmpty,
              selector.kind != .bundle_id || identifier == selector.value else {
            throw DesktopFailure(code: "application_target_invalid", message: "Resolved target is not the exact application bundle")
        }
        let instances = running.filter { $0.bundle_path == url.path && $0.bundle_id == identifier }
        guard instances.count <= 1 else { throw DesktopFailure(code: "application_instance_ambiguous", message: "The exact bundle has multiple running instances") }
        let instance = instances.first.map { DesktopApplicationInstance(app: $0.app, process_id: $0.process_id, launch_time_unix_ms: $0.launch_time_unix_ms) }
        let displayName = Self.displayName(bundle, url: url)
        let target = DesktopApplicationTarget(bundle_id: identifier, bundle_path: url.path, display_name: displayName, instance: instance)
        let nativePolicy = policy?.0.resolve(target, bundle: bundle)
        if let existing = targets.first(where: { $0.value.0 == url && $0.value.1 == target && $0.value.2 == nativePolicy }) {
            return DesktopPreparedApplication(target_handle: existing.key, application: target, native_policy: nativePolicy, native_policy_digest: policy?.1)
        }
        guard targets.count < 1024 else { throw DesktopFailure(code: "application_target_limit", message: "Native target inventory exceeds its bound") }
        let handle = UUID().uuidString.lowercased()
        targets[handle] = (url, target, nativePolicy)
        return DesktopPreparedApplication(target_handle: handle, application: target, native_policy: nativePolicy, native_policy_digest: policy?.1)
    }

    func openPreparedTarget(_ handle: String, check: @escaping () throws -> Void) async throws -> DesktopAppInfo {
        try requireOpen(); try check()
        try validatePolicy(handle)
        guard let (url, target, nativePolicy) = targets[handle], url.standardizedFileURL.resolvingSymlinksInPath().path == target.bundle_path,
              Bundle(url: url)?.bundleIdentifier == target.bundle_id else {
            throw DesktopFailure(code: "application_target_changed", message: "The retained approved bundle is no longer the same target")
        }
        if let instance = target.instance {
            let current = try info(instance.app)
            guard current.process_id == instance.process_id, current.bundle_path == target.bundle_path, current.bundle_id == target.bundle_id else {
                throw DesktopFailure(code: "app_instance_unavailable", message: "Approved native application instance changed")
            }
            try check()
            return current
        }
        let configuration = NSWorkspace.OpenConfiguration()
        configuration.activates = false
        configuration.allowsRunningApplicationSubstitution = false
        try check()
        let opened: NSRunningApplication = try await withCheckedThrowingContinuation { continuation in
            NSWorkspace.shared.open(url, configuration: configuration) { application, error in
                if let error { continuation.resume(throwing: error) }
                else if let application { continuation.resume(returning: application) }
                else { continuation.resume(throwing: DesktopFailure(code: "application_launch_unavailable", message: "Application open returned no native instance")) }
            }
        }
        try requireOpen(); try check(); try validatePolicy(handle)
        guard !opened.isTerminated, opened.bundleIdentifier == target.bundle_id,
              opened.bundleURL?.standardizedFileURL.resolvingSymlinksInPath() == url else {
            throw DesktopFailure(code: "application_launch_mismatch", message: "Application open did not return the exact approved bundle")
        }
        let current = try list().filter { $0.process_id == opened.processIdentifier && $0.bundle_id == target.bundle_id && $0.bundle_path == target.bundle_path }
        guard current.count == 1, let info = current.first, try application(info.app).isEqual(opened) else {
            throw DesktopFailure(code: "application_launch_mismatch", message: "Opened native instance cannot be retained exactly")
        }
        try check()
        targets[handle] = (url, DesktopApplicationTarget(bundle_id: target.bundle_id, bundle_path: target.bundle_path, display_name: target.display_name,
            instance: DesktopApplicationInstance(app: info.app, process_id: info.process_id, launch_time_unix_ms: info.launch_time_unix_ms)), nativePolicy)
        return info
    }

    func validatePolicy(_ handle: String) throws {
        try requireOpen()
        guard let (url, target, snapshot) = targets[handle], let bundle = Bundle(url: url),
              bundle.bundleIdentifier == target.bundle_id, Self.displayName(bundle, url: url) == target.display_name,
              url.standardizedFileURL.resolvingSymlinksInPath().path == target.bundle_path else {
            throw DesktopFailure(code: "application_target_changed", message: "The retained application bundle metadata changed")
        }
        let current = policy?.0.resolve(target, bundle: bundle)
        try current?.requireAllowed()
        guard current == snapshot else {
            throw DesktopFailure(code: "application_policy_changed", message: "Native application eligibility changed after target resolution")
        }
    }

    private static func displayName(_ bundle: Bundle?, url: URL) -> String {
        (bundle?.object(forInfoDictionaryKey: "CFBundleDisplayName") as? String)
            ?? (bundle?.object(forInfoDictionaryKey: "CFBundleName") as? String)
            ?? url.deletingPathExtension().lastPathComponent
    }

    func info(_ reference: DesktopAppRef) throws -> DesktopAppInfo {
        _ = try application(reference)
        guard let entry = entries[reference.app_instance_id] else {
            throw DesktopFailure(code: "app_instance_unavailable", message: "Application reference was withdrawn")
        }
        return info(entry)
    }

    func close() { closed = true; catalog?.close(); catalog = nil; entries.removeAll(); targets.removeAll() }

    private func requireOpen() throws {
        guard !closed else { throw DesktopFailure(code: "app_scope_closed", message: "Native application scope was released") }
    }

    private func retireExited() {
        entries = entries.filter { _, entry in
            guard !entry.application.isTerminated, let current = NSRunningApplication(processIdentifier: entry.processID) else { return false }
            return current.isEqual(entry.application) && current.processIdentifier == entry.processID
        }
    }

    private func info(_ entry: Entry) -> DesktopAppInfo {
        let launch = entry.application.launchDate.map { $0.timeIntervalSince1970 * 1000 }.flatMap { $0.isFinite && $0 >= 1 && $0 < Double(UInt64.max) ? UInt64($0) : nil }
        return DesktopAppInfo(app: entry.reference, bundle_id: entry.bundleID, bundle_path: entry.bundlePath,
                              display_name: Self.displayName(Bundle(url: URL(fileURLWithPath: entry.bundlePath)), url: URL(fileURLWithPath: entry.bundlePath)), process_id: entry.processID,
                              launch_time_unix_ms: launch, active: entry.application.isActive, hidden: entry.application.isHidden)
    }
}
