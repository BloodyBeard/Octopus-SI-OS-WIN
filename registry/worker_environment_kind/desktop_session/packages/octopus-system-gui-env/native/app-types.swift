import Foundation

struct DesktopAppRef: Codable, Equatable, Hashable {
    let host_generation: String
    let app_instance_id: String
}

struct DesktopAXAction: Codable, Equatable {
    let action_id: String
    let name: String
    let description: String?
}
