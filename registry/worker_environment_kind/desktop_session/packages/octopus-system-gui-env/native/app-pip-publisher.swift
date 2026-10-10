import AppKit
import CryptoKit
import Darwin
import Foundation
import OctopusDesktopProducerBridge
import QuartzCore
import Security

struct DesktopPIPScope: Codable, Equatable, Hashable {
    let host_generation: String
    let worker_environment_id: String
    let formation_pin: String
    let app_instance_id: String
    let grant_request_id: String
    var room: DesktopAppScope { DesktopAppScope(host_generation: host_generation, worker_environment_id: worker_environment_id, formation_pin: formation_pin) }
    var app: DesktopAppRef { DesktopAppRef(host_generation: host_generation, app_instance_id: app_instance_id) }
}

@MainActor
final class DesktopAppPIPSource {
    let scope: DesktopPIPScope
    let windowID: String
    let source: [String: Any]
    let snapshot: () throws -> DesktopAppPIPRenderer.Snapshot
    let cursor: () throws -> DesktopAppPIPRenderer.CursorSnapshot?
    let observe: (@escaping (DesktopAppPIPRenderer.Event) -> Void) throws -> DesktopAppPIPObservation?
    let humanFocus: () throws -> Void
    init(scope: DesktopPIPScope, windowID: String, source: [String: Any],
         snapshot: @escaping () throws -> DesktopAppPIPRenderer.Snapshot,
         cursor: @escaping () throws -> DesktopAppPIPRenderer.CursorSnapshot?,
         observe: @escaping (@escaping (DesktopAppPIPRenderer.Event) -> Void) throws -> DesktopAppPIPObservation?,
         humanFocus: @escaping () throws -> Void) {
        self.scope=scope; self.windowID=windowID; self.source=source
        self.snapshot=snapshot; self.cursor=cursor; self.observe=observe; self.humanFocus=humanFocus
    }
}

private struct DesktopPIPBinding {
    let object: [String: Any]
    let scope: DesktopPIPScope
    let bootstrapID: String
    let requestID: String
    let receiverNonce: String
    let instanceNonce: String
    let presentationID: String
    let generation: UInt64
    let windowID: String
    let incarnation: String
    let cgWindowID: UInt32
    let receiverPID: Int32
    let producerPID: Int32
    let deadline: UInt64
    let source: [String: Any]
    var identity: [String: Any] { ["hostGeneration":scope.host_generation,"presentationId":presentationID,"streamGeneration":generation] }
    var channelKey: String { "\(scope.host_generation):\(producerPID):\(receiverPID):\(instanceNonce)" }
    static let keys: Set<String> = ["schema_version","host_generation","bootstrap_id","request_id","receiver_nonce",
        "receiver_instance_nonce","scope","source","presentation_id","stream_generation","source_window_id",
        "native_window_incarnation","native_cg_window_id","receiver_pid","producer_pid","expires_at_mach_continuous_ticks"]

    init(_ bytes: Data) throws {
        let value=try DesktopPIPJSON.object(bytes)
        guard Set(value.keys)==Self.keys, try Self.integer(value,"schema_version")==1 else { throw Self.invalid() }
        let scopeValue=try Self.dictionary(value,"scope")
        guard Set(scopeValue.keys)==["host_generation","worker_environment_id","formation_pin","app_instance_id","grant_request_id"] else { throw Self.invalid() }
        scope=try JSONDecoder().decode(DesktopPIPScope.self,from:JSONSerialization.data(withJSONObject:scopeValue))
        try scope.room.validate()
        for key in scopeValue.keys { _=try Self.string(scopeValue,key) }
        guard try Self.string(value,"host_generation")==scope.host_generation else { throw Self.invalid() }
        bootstrapID=try Self.hex(value,"bootstrap_id",count:32)
        requestID=try Self.string(value,"request_id")
        receiverNonce=try Self.hex(value,"receiver_nonce",count:64)
        instanceNonce=try Self.hex(value,"receiver_instance_nonce",count:64)
        presentationID=try Self.string(value,"presentation_id")
        generation=try Self.integer(value,"stream_generation")
        windowID=try Self.string(value,"source_window_id")
        incarnation=try Self.string(value,"native_window_incarnation")
        guard let inc=UInt64(incarnation), inc>0, String(inc)==incarnation else { throw Self.invalid() }
        let cg=try Self.integer(value,"native_cg_window_id"),receiver=try Self.integer(value,"receiver_pid"),producer=try Self.integer(value,"producer_pid")
        guard cg<=UInt32.max, receiver<=Int32.max, producer<=Int32.max else { throw Self.invalid() }
        cgWindowID=UInt32(cg);receiverPID=Int32(receiver);producerPID=Int32(producer)
        let expiry=try Self.string(value,"expires_at_mach_continuous_ticks")
        guard let ticks=UInt64(expiry), ticks>0, String(ticks)==expiry else { throw Self.invalid() }
        deadline=ticks;source=try Self.dictionary(value,"source");object=value
    }
    static func invalid() -> DesktopFailure { DesktopFailure(code:"pip_binding_invalid",message:"Native presentation binding does not match its exact private schema") }
    static func string(_ value:[String:Any],_ key:String)throws->String {
        guard let text=value[key] as? String,!text.isEmpty,text.utf8.count<=4096,!text.contains("\0") else { throw invalid() };return text
    }
    static func hex(_ value:[String:Any],_ key:String,count:Int)throws->String {
        let text=try string(value,key)
        guard text.utf8.count==count,text.utf8.allSatisfy({ (48...57).contains($0)||(97...102).contains($0) }) else { throw invalid() };return text
    }
    static func base64(_ value:[String:Any],_ key:String,maximum:Int)throws->Data {
        guard let encoded=value[key] as? String,encoded.utf8.count<=((maximum+2)/3)*4,
              let decoded=Data(base64Encoded:encoded),decoded.count<=maximum,
              decoded.base64EncodedString()==encoded else{throw invalid()};return decoded
    }
    static func dictionary(_ value:[String:Any],_ key:String)throws->[String:Any] {
        guard let result=value[key] as? [String:Any] else { throw invalid() };return result
    }
    static func integer(_ value:[String:Any],_ key:String)throws->UInt64 {
        guard let number=value[key] as? NSNumber,CFGetTypeID(number) != CFBooleanGetTypeID(),
              number.doubleValue.isFinite,number.doubleValue>=1,number.doubleValue<=9_007_199_254_740_991,
              number.doubleValue.rounded(.towardZero)==number.doubleValue else { throw invalid() };return number.uint64Value
    }
}

private struct DesktopPIPCompletion: Equatable {
    let scope: DesktopPIPScope
    let windowID: String
    let presentationID: String
    let generation: UInt64
    let instanceNonce: String
    var key: String { "\(presentationID):\(generation)" }
    var target: [String: Any] {
        ["scope":["host_generation":scope.host_generation,"worker_environment_id":scope.worker_environment_id,
                  "formation_pin":scope.formation_pin,"app_instance_id":scope.app_instance_id,"grant_request_id":scope.grant_request_id],
         "source_window_id":windowID,"presentation_id":presentationID,"stream_generation":generation,
         "receiver_instance_nonce":instanceNonce]
    }
    init(_ action: [String: Any]) throws {
        guard Set(action.keys)==["type","scope","source_window_id","presentation_id","stream_generation","receiver_instance_nonce"] else { throw DesktopPIPBinding.invalid() }
        let value=try DesktopPIPBinding.dictionary(action,"scope")
        guard Set(value.keys)==["host_generation","worker_environment_id","formation_pin","app_instance_id","grant_request_id"] else { throw DesktopPIPBinding.invalid() }
        scope=try JSONDecoder().decode(DesktopPIPScope.self,from:JSONSerialization.data(withJSONObject:value))
        try scope.room.validate()
        for key in value.keys { _=try DesktopPIPBinding.string(value,key) }
        windowID=try DesktopPIPBinding.string(action,"source_window_id")
        presentationID=try DesktopPIPBinding.string(action,"presentation_id")
        generation=try DesktopPIPBinding.integer(action,"stream_generation")
        instanceNonce=try DesktopPIPBinding.hex(action,"receiver_instance_nonce",count:64)
    }
    func matches(_ binding: DesktopPIPBinding) -> Bool {
        scope==binding.scope && windowID==binding.windowID && presentationID==binding.presentationID
            && generation==binding.generation && instanceNonce==binding.instanceNonce
    }
    func result(_ status: String) -> [String: Any] { ["schema_version":1,"status":status,"target":target] }
}

private enum DesktopPIPJSON {
    static func object(_ data:Data)throws->[String:Any] {
        guard !data.isEmpty,data.count<=65536,data.first != 0xef else { throw DesktopPIPBinding.invalid() }
        var scanner=Scanner(bytes:Array(data));try scanner.value(0);scanner.whitespace()
        guard scanner.offset==data.count,let value=try JSONSerialization.jsonObject(with:data) as? [String:Any] else { throw DesktopPIPBinding.invalid() };return value
    }
    private struct Scanner {
        let bytes:[UInt8];var offset=0
        mutating func whitespace(){while offset<bytes.count && [9,10,13,32].contains(bytes[offset]){offset+=1}}
        mutating func string()throws->String {
            guard offset<bytes.count,bytes[offset]==34 else{throw DesktopPIPBinding.invalid()}
            let start=offset;offset+=1
            while offset<bytes.count {
                let byte=bytes[offset];offset+=1
                if byte==34 {
                    guard let value=try JSONSerialization.jsonObject(with:Data(bytes[start..<offset]),options:.fragmentsAllowed) as? String else{throw DesktopPIPBinding.invalid()};return value
                }
                if byte==92 {guard offset<bytes.count else{throw DesktopPIPBinding.invalid()};offset+=1}
            }
            throw DesktopPIPBinding.invalid()
        }
        mutating func value(_ depth:Int)throws {
            guard depth<64 else{throw DesktopPIPBinding.invalid()};whitespace();guard offset<bytes.count else{throw DesktopPIPBinding.invalid()}
            if bytes[offset]==34 {_=try string();return}
            if bytes[offset]==123 {
                offset+=1;whitespace();var keys:Set<String>=[]
                if offset<bytes.count,bytes[offset]==125{offset+=1;return}
                while true {
                    whitespace();let key=try string();guard keys.insert(key).inserted else{throw DesktopPIPBinding.invalid()};whitespace()
                    guard offset<bytes.count,bytes[offset]==58 else{throw DesktopPIPBinding.invalid()};offset+=1;try value(depth+1);whitespace()
                    guard offset<bytes.count else{throw DesktopPIPBinding.invalid()};let byte=bytes[offset];offset+=1
                    if byte==125{return};guard byte==44 else{throw DesktopPIPBinding.invalid()}
                }
            }
            if bytes[offset]==91 {
                offset+=1;whitespace();if offset<bytes.count,bytes[offset]==93{offset+=1;return}
                while true {try value(depth+1);whitespace();guard offset<bytes.count else{throw DesktopPIPBinding.invalid()};let byte=bytes[offset];offset+=1;if byte==93{return};guard byte==44 else{throw DesktopPIPBinding.invalid()}}
            }
            let start=offset
            while offset<bytes.count && ![9,10,13,32,44,93,125].contains(bytes[offset]){offset+=1}
            guard offset>start else{throw DesktopPIPBinding.invalid()}
        }
    }
}

@MainActor
final class DesktopAppPIPPublisher {
    private final class Desired {
        let id=UUID().uuidString.lowercased()
        let scope:DesktopPIPScope
        let windowID:String
        let incarnation:String
        var nextGeneration:UInt64=0
        init(_ source:DesktopAppPIPSource,_ snapshot:DesktopAppPIPRenderer.Snapshot){scope=source.scope;windowID=source.windowID;incarnation=snapshot.primary.incarnation}
    }
    private final class Pending {
        enum Stage{case prepared,installed,reserved,authenticated,active,terminal}
        let expected:[String:Any]
        let source:DesktopAppPIPSource
        let desired:Desired
        let deadline:UInt64
        var stage:Stage = .prepared
        var binding:DesktopPIPBinding?
        var bytes=Data()
        var key=Data()
        var receiverChallenge=Data()
        var producerChallenge=Data()
        var connection:NSXPCConnection?
        var expiry:Task<Void,Never>?
        init(expected:[String:Any],source:DesktopAppPIPSource,desired:Desired,deadline:UInt64){self.expected=expected;self.source=source;self.desired=desired;self.deadline=deadline}
        func erase(){key.resetBytes(in:0..<key.count);key.removeAll();receiverChallenge.removeAll();producerChallenge.removeAll();expiry?.cancel();expiry=nil}
    }
    private final class Publication {
        let pending:Pending
        let binding:DesktopPIPBinding
        let peer:NSXPCConnection
        var renderer:DesktopAppPIPRenderer?
        var context:OCTPIPProducerContext?
        var serial:Task<Void,Never>?
        var ending:Task<Void,Never>?
        var operation:UInt64=0
        var size=CGSize(width:200,height:200)
        var published=false
        var disposed=false
        var endingSource=false
        var endingAcknowledged=false
        var peerEnded=false
        var completion:Task<[String:Any],Error>?
        var queued=0
        init(_ pending:Pending,_ binding:DesktopPIPBinding,_ peer:NSXPCConnection){self.pending=pending;self.binding=binding;self.peer=peer}
    }
    private let resolve:(DesktopPIPScope,String)throws->DesktopAppPIPSource
    private let diagnostic:(String,String,String)->Void
    private var transport:OCTPIPProducerTransport?
    private var desired:[String:Desired]=[:]
    private var pending:[String:Pending]=[:]
    private var seenRequests:Set<String>=[]
    private var seenBootstraps:Set<String>=[]
    private var established:Set<ObjectIdentifier>=[]
    private var teardowns:[UUID:Task<Void,Never>]=[:]
    private var channels:[String:NSXPCConnection]=[:]
    private var publications:[String:Publication]=[:]
    private var completed:[String:(DesktopPIPCompletion,String)]=[:]
    private var closed=false

    init(resolve:@escaping(DesktopPIPScope,String)throws->DesktopAppPIPSource,diagnostic:@escaping(String,String,String)->Void){self.resolve=resolve;self.diagnostic=diagnostic}
    func prepare(_ action:[String:Any])throws->[String:Any] {
        try checkOpen()
        guard Set(action.keys)==["type","bootstrap_id","request_id","scope","source","source_window_id","receiver_pid","receiver_nonce","receiver_instance_nonce","expires_at_mach_continuous_ticks"] else{throw DesktopPIPBinding.invalid()}
        let scopeValue=try DesktopPIPBinding.dictionary(action,"scope")
        guard Set(scopeValue.keys)==["host_generation","worker_environment_id","formation_pin","app_instance_id","grant_request_id"] else{throw DesktopPIPBinding.invalid()}
        let scope=try JSONDecoder().decode(DesktopPIPScope.self,from:JSONSerialization.data(withJSONObject:scopeValue))
        let id=try DesktopPIPBinding.hex(action,"bootstrap_id",count:32)
        let request=try DesktopPIPBinding.string(action,"request_id")
        let nonce=try DesktopPIPBinding.hex(action,"receiver_nonce",count:64)
        _=try DesktopPIPBinding.hex(action,"receiver_instance_nonce",count:64)
        let receiver=try DesktopPIPBinding.integer(action,"receiver_pid")
        guard receiver<=Int32.max,Int32(receiver) != getpid() else{throw DesktopPIPBinding.invalid()}
        let expiry=try DesktopPIPBinding.string(action,"expires_at_mach_continuous_ticks")
        guard let deadline=UInt64(expiry),String(deadline)==expiry,deadline>mach_continuous_time() else{throw DesktopPIPBinding.invalid()}
        let key="\(scope.host_generation):\(request):\(nonce)"
        guard pending[id]==nil,!seenBootstraps.contains(id),!seenRequests.contains(key),seenRequests.count<4096,pending.count<256 else{throw DesktopPIPBinding.invalid()}
        let window=try DesktopPIPBinding.string(action,"source_window_id")
        let source=try resolve(scope,window),snapshot=try source.snapshot()
        guard NSDictionary(dictionary:source.source).isEqual(to:try DesktopPIPBinding.dictionary(action,"source")) else{throw DesktopPIPBinding.invalid()}
        let sourceKey="\(scope.worker_environment_id):\(scope.formation_pin):\(scope.app_instance_id):\(window)"
        let current:Desired
        if let existing=desired[sourceKey] {
            guard existing.scope==scope,existing.incarnation==snapshot.primary.incarnation else{throw DesktopFailure(code:"pip_source_changed",message:"A native source cannot adopt another window incarnation")};current=existing
        }else{
            guard desired.count<256 else{throw DesktopFailure(code:"pip_source_limit",message:"Native presentation inventory exceeded its bound")}
            current=Desired(source,snapshot);desired[sourceKey]=current
        }
        guard current.nextGeneration<9_007_199_254_740_991 else{throw DesktopPIPBinding.invalid()};current.nextGeneration+=1
        var expected=action;expected.removeValue(forKey:"type");expected["schema_version"]=1;expected["host_generation"]=scope.host_generation
        expected["presentation_id"]=current.id;expected["stream_generation"]=current.nextGeneration
        expected["native_window_incarnation"]=snapshot.primary.incarnation;expected["native_cg_window_id"]=snapshot.primary.descriptor.cg_window_id
        expected["producer_pid"]=getpid()
        let entry=Pending(expected:expected,source:source,desired:current,deadline:deadline);pending[id]=entry;seenRequests.insert(key);seenBootstraps.insert(id)
        entry.expiry=Task{[weak self,weak entry] in
            guard entry != nil else{return}
            do{try await Task.sleep(nanoseconds:try Self.remaining(deadline));try Task.checkCancellation();self?.cancel(id)}catch{}
        }
        return ["schema_version":1,"producer_pid":getpid(),"presentation_id":current.id,"stream_generation":current.nextGeneration,
                "native_window_incarnation":snapshot.primary.incarnation,"native_cg_window_id":snapshot.primary.descriptor.cg_window_id]
    }
    func install(_ action:[String:Any])throws->[String:Any] {
        try checkOpen()
        guard Set(action.keys)==["type","schema_version","bootstrap_id","bootstrap_key_base64","binding_bytes_base64"],
              try DesktopPIPBinding.integer(action,"schema_version")==1 else{throw DesktopPIPBinding.invalid()}
        let id=try DesktopPIPBinding.hex(action,"bootstrap_id",count:32)
        let bytes=try DesktopPIPBinding.base64(action,"binding_bytes_base64",maximum:65536)
        let key=try DesktopPIPBinding.base64(action,"bootstrap_key_base64",maximum:32)
        guard let entry=pending[id],entry.stage == .prepared,key.count==32 else{throw DesktopPIPBinding.invalid()}
        let binding=try DesktopPIPBinding(bytes)
        guard binding.bootstrapID==id,binding.producerPID==getpid(),NSDictionary(dictionary:entry.expected).isEqual(to:binding.object) else{throw DesktopPIPBinding.invalid()}
        try validate(entry)
        entry.binding=binding;entry.bytes=bytes;entry.key=key;entry.stage = .installed
        do{try startTransport()}catch{cancel(id);throw error}
        return ["schema_version":1,"bootstrap_id":id,"installed":true,"producer_pid":getpid()]
    }
    func complete(_ action:[String:Any])async throws->[String:Any] {
        try checkOpen()
        let target=try DesktopPIPCompletion(action)
        let snapshot=try resolve(target.scope,target.windowID).snapshot()
        if let (previous,incarnation)=completed[target.key] {
            guard previous==target,incarnation==snapshot.primary.incarnation else{throw DesktopPIPBinding.invalid()}
            return target.result("ended")
        }
        guard let publication=publications.values.first(where:{$0.binding.presentationID==target.presentationID && $0.binding.generation==target.generation}) else {
            if let entry=pending.values.first(where:{$0.binding?.presentationID==target.presentationID && $0.binding?.generation==target.generation}) {
                guard let binding=entry.binding,target.matches(binding) else{throw DesktopPIPBinding.invalid()}
                throw DesktopFailure(code:"pip_source_not_active",message:"Native presentation source has not completed authentication")
            }
            for value in desired.values where value.id==target.presentationID {
                guard value.scope==target.scope,value.windowID==target.windowID,value.incarnation==snapshot.primary.incarnation else{throw DesktopPIPBinding.invalid()}
            }
            return target.result("absent")
        }
        guard target.matches(publication.binding),snapshot.primary.incarnation==publication.binding.incarnation else{throw DesktopPIPBinding.invalid()}
        _=try receiver(publication)
        if let completion=publication.completion{return try await completion.value}
        guard completed.count<4096 else{throw DesktopFailure(code:"pip_completion_limit",message:"Native presentation completion history reached its finite bound")}
        publication.endingSource=true
        let previous=publication.serial
        let completion=Task{[self,publication] in
            await previous?.value
            do {
                try Task.checkCancellation()
                guard publication.published else{throw DesktopFailure(code:"pip_source_not_published",message:"Native receiver has not acknowledged this publication")}
                let receiver=try receiver(publication)
                try await reply(publication){receiver.invalidatePresentation(publication.binding.identity,reason:"completed",reply:$0)}
                let current=try resolve(target.scope,target.windowID).snapshot()
                guard current.primary.incarnation==publication.binding.incarnation else{throw DesktopPIPBinding.invalid()}
                publication.endingAcknowledged=true
                completed[target.key]=(target,publication.binding.incarnation)
                forgetDesired(publication)
                diagnostic(target.presentationID,"ending","receiver_invalidation_acknowledged")
                if publication.peerEnded { cancel(publication.binding.bootstrapID) }
                else {
                    publication.ending=Task{[weak self,weak publication] in
                        do{try await Task.sleep(nanoseconds:1_000_000_000);try Task.checkCancellation()
                            guard let self,let publication else{return};self.cancel(publication.binding.bootstrapID)
                        }catch{}
                    }
                }
                return target.result("ended")
            } catch {
                diagnostic(target.presentationID,"failed",(error as? DesktopFailure)?.code ?? "pip_completion_uncertain")
                forgetDesired(publication);cancel(publication.binding.bootstrapID)
                throw error
            }
        }
        publication.completion=completion
        return try await completion.value
    }
    private func forgetDesired(_ publication:Publication) {
        desired=desired.filter{ !($0.value === publication.pending.desired && $0.value.nextGeneration==publication.binding.generation) }
    }
    func cancel(_ id:String){
        guard let entry=pending[id] else{return}
        entry.stage = .terminal;entry.erase()
        if let publication=publications.removeValue(forKey:id){dispose(publication)}
        pending.removeValue(forKey:id)
        if let connection=entry.connection,!established.contains(ObjectIdentifier(connection)){transport?.close(connection)}
    }
    func revoke(scope:DesktopAppScope,app:DesktopAppRef?=nil)async{
        for (id,entry) in pending where entry.source.scope.room==scope && (app==nil || entry.source.scope.app==app){cancel(id)}
        desired=desired.filter{$0.value.scope.room != scope || (app != nil && $0.value.scope.app != app!)}
        let closing=Array(teardowns.values);for task in closing{await task.value}
    }
    func changed(scope:DesktopAppScope,app:DesktopAppRef){
        for publication in publications.values where publication.binding.scope.room==scope && publication.binding.scope.app==app && !publication.endingSource {
            do{try publication.renderer?.updateAssociatedWindows();publication.renderer?.cursorDidChange()}
            catch{withdraw(publication,error:error)}
        }
    }
    func noteInteraction(scope:DesktopAppScope,app:DesktopAppRef){
        for publication in publications.values where publication.binding.scope.room==scope && publication.binding.scope.app==app && publication.published && !publication.endingSource {
            enqueue(publication){[weak self] in guard let self else{return};let receiver=try self.receiver(publication);try await self.reply(publication){receiver.noteInteraction(publication.binding.identity,reply:$0)}}
        }
    }
    func close()async{
        guard !closed else{return};closed=true
        for entry in pending.values{entry.erase()}
        for publication in publications.values{dispose(publication)}
        publications.removeAll();pending.removeAll();desired.removeAll();completed.removeAll();channels.removeAll();established.removeAll();transport?.close();transport=nil
        let closing=Array(teardowns.values);for task in closing{await task.value}
    }
    private func startTransport()throws{
        if transport != nil{return}
        let created=OCTPIPProducerTransport(factory:{[weak self] peer in
            MainActor.assumeIsolated{guard let self,!self.closed,self.pending.values.contains(where:{
                $0.stage == .installed && $0.binding?.receiverPID==peer.processIdentifier && $0.deadline>mach_continuous_time()
            }) else{return nil};return DesktopPIPPublisherEndpoint(owner:self,peer:peer)}
        },disconnected:{[weak self] peer in MainActor.assumeIsolated{self?.disconnected(peer)}},deadlineForPID:{[weak self] pid in
            MainActor.assumeIsolated{self?.pending.values.filter{$0.stage == .installed && $0.binding?.receiverPID==pid}.map(\.deadline).max() ?? 0}
        })
        try created.start();transport=created
    }
    fileprivate func connect(_ peer:NSXPCConnection)throws{
        try checkOpen()
        guard pending.values.contains(where:{$0.stage == .installed && $0.binding?.receiverPID==peer.processIdentifier && $0.deadline>mach_continuous_time()}) else{throw DesktopPIPBinding.invalid()}
    }
    fileprivate func authenticationFailed(_ peer:NSXPCConnection,id:String?){
        if let id,let entry=pending[id],entry.connection === peer{cancel(id)}
        if !established.contains(ObjectIdentifier(peer)){transport?.close(peer)}
    }
    fileprivate func begin(_ bytes:Data,challenge:Data,peer:NSXPCConnection)throws->(Data,Data){
        try checkOpen();let binding=try DesktopPIPBinding(bytes)
        guard let entry=pending[binding.bootstrapID],entry.stage == .installed,entry.bytes==bytes,challenge.count==32,
              binding.receiverPID==peer.processIdentifier,binding.producerPID==getpid(),
              channels[binding.channelKey]==nil || channels[binding.channelKey] === peer else{throw DesktopPIPBinding.invalid()}
        try validate(entry)
        entry.connection=peer;entry.receiverChallenge=challenge
        var random=Data(count:32)
        guard random.withUnsafeMutableBytes({SecRandomCopyBytes(kSecRandomDefault,32,$0.baseAddress!)})==errSecSuccess else{cancel(binding.bootstrapID);throw DesktopPIPBinding.invalid()}
        entry.producerChallenge=random;entry.stage = .reserved
        return(random,proof(entry,role:2))
    }
    fileprivate func finish(_ id:String,proof received:Data,peer:NSXPCConnection)throws{
        guard let entry=pending[id],let binding=entry.binding,entry.stage == .reserved,entry.connection === peer,
              binding.receiverPID==peer.processIdentifier,received.count==32,
              channels[binding.channelKey]==nil || channels[binding.channelKey] === peer else{throw DesktopPIPBinding.invalid()}
        try validate(entry)
        guard OCTPIPConstantTimeEqual(proof(entry,role:1),received) else{cancel(id);throw DesktopPIPBinding.invalid()}
        entry.stage = .authenticated;entry.key.resetBytes(in:0..<entry.key.count);entry.key.removeAll()
        entry.receiverChallenge.removeAll();entry.producerChallenge.removeAll()
        channels[binding.channelKey]=peer
    }
    fileprivate func activate(_ id:String,peer:NSXPCConnection)throws{
        guard let entry=pending[id],let binding=entry.binding,entry.stage == .authenticated,entry.connection === peer,
              channels[binding.channelKey] === peer,binding.receiverPID==peer.processIdentifier else{throw DesktopPIPBinding.invalid()}
        try validate(entry);entry.stage = .active;entry.expiry?.cancel();entry.expiry=nil
        established.insert(ObjectIdentifier(peer))
        for (oldID,old) in publications where old.binding.presentationID==binding.presentationID && old.peer === peer{cancel(oldID)}
        let publication=Publication(entry,binding,peer);publications[id]=publication
        enqueue(publication){[weak self] in try await self?.publish(publication)}
    }
    fileprivate func peerCancel(_ id:String,peer:NSXPCConnection)throws{
        guard let entry=pending[id],entry.binding?.receiverPID==peer.processIdentifier,
              entry.connection==nil || entry.connection === peer else{throw DesktopPIPBinding.invalid()};cancel(id)
    }
    fileprivate func ended(_ identity:[String:Any],peer:NSXPCConnection)throws{
        let publication=try exact(identity,peer:peer)
        guard publication.endingSource else{return}
        publication.peerEnded=true
        if publication.endingAcknowledged{cancel(publication.binding.bootstrapID)}
    }
    fileprivate func focus(_ identity:[String:Any],peer:NSXPCConnection)throws{
        let publication=try exact(identity,peer:peer);_ = try publication.pending.source.snapshot();try publication.pending.source.humanFocus()
    }
    fileprivate func maximum(_ maximum:Double,identity:[String:Any],peer:NSXPCConnection)throws{
        guard maximum.isFinite else{throw DesktopPIPBinding.invalid()};let publication=try exact(identity,peer:peer)
        enqueue(publication){[weak self] in guard let self,let renderer=publication.renderer else{return};let size=try renderer.setMaximumDisplaySize(CGFloat(maximum));try await self.resize(publication,size:size,kind:"resize")}
    }
    private func validate(_ entry:Pending)throws{
        try checkOpen()
        if entry.stage != .active && mach_continuous_time()>=entry.deadline{throw DesktopFailure(code:"pip_bootstrap_expired",message:"Native presentation authentication expired")}
        let snapshot=try entry.source.snapshot()
        guard snapshot.primary.descriptor.window_id==entry.desired.windowID,snapshot.primary.incarnation==entry.desired.incarnation,
              snapshot.primary.descriptor.app==entry.source.scope.app else{throw DesktopPIPBinding.invalid()}
    }
    private func checkOpen()throws{
        guard !closed,OctopusDesktopNative.parentChannel.isOpen else{throw DesktopFailure(code:"pip_owner_closed",message:"Native presentation owner was withdrawn")}
    }
    private func proof(_ entry:Pending,role:UInt8)->Data{
        var transcript=Data("octopus.native-pip.v1.auth\n".utf8);var count=UInt32(entry.bytes.count).bigEndian
        withUnsafeBytes(of:&count){transcript.append(contentsOf:$0)}
        transcript.append(entry.bytes);transcript.append(entry.receiverChallenge);transcript.append(entry.producerChallenge);transcript.append(role)
        return Data(HMAC<SHA256>.authenticationCode(for:transcript,using:SymmetricKey(data:entry.key)))
    }
    private func exact(_ value:[String:Any],peer:NSXPCConnection)throws->Publication{
        guard Set(value.keys)==["hostGeneration","presentationId","streamGeneration"] else{throw DesktopPIPBinding.invalid()}
        guard let publication=publications.values.first(where:{NSDictionary(dictionary:$0.binding.identity).isEqual(to:value)}),
              publication.peer === peer,publication.binding.receiverPID==peer.processIdentifier,
              channels[publication.binding.channelKey] === peer,!publication.disposed else{throw DesktopPIPBinding.invalid()}
        try validate(publication.pending);return publication
    }
    private func receiver(_ publication:Publication)throws->OCTPIPReceiver{
        try validate(publication.pending)
        guard !publication.disposed,channels[publication.binding.channelKey] === publication.peer,let transport else{throw DesktopPIPBinding.invalid()}
        return try transport.receiver(for:publication.peer,expectedPID:publication.binding.receiverPID)
    }
    private func enqueue(_ publication:Publication,operation:@escaping()async throws->Void){
        guard !publication.endingSource else{return}
        guard publication.queued<64 else{withdraw(publication,error:DesktopFailure(code:"pip_operation_limit",message:"Native presentation operation queue exceeded its bound"));return}
        publication.queued+=1
        let previous=publication.serial
        publication.serial=Task{[weak self,weak publication] in
            await previous?.value
            defer{if let publication{publication.queued-=1}}
            guard let self,let publication,!Task.isCancelled,!publication.disposed else{return}
            do{try await operation()}catch is CancellationError{}catch{self.withdraw(publication,error:error)}
        }
    }
    private func publish(_ publication:Publication)async throws{
        let source=publication.pending.source;let snapshot=try source.snapshot()
        let context=try OCTPIPProducerContext.make();publication.context=context
        let renderer=try DesktopAppPIPRenderer(application:snapshot.application,source:snapshot.primary,maximumDisplaySize:200,
            check:source.snapshot,cursor:source.cursor,observe:source.observe,
            callbacks:.init(sourceResizeChanged:{[weak self,weak publication] size in
                guard let self,let publication else{return};self.enqueue(publication){try await self.resize(publication,size:size,kind:"resize-in-progress")}
            },sourceResizeSettled:{[weak self,weak publication] size in
                guard let self,let publication else{return};self.enqueue(publication){try await self.resize(publication,size:size,kind:"resize")}
            },sourceUnavailable:{[weak self,weak publication] error in
                guard let self,let publication else{return};self.withdraw(publication,error:error)
            },diagnostic:{[weak self,weak publication] code,_ in
                guard let self,let publication else{return};self.diagnostic(publication.binding.presentationID,"warning",code)
            }))
        publication.renderer=renderer
        let size=try await renderer.start(in:context.rootLayer)
        let receiver=try receiver(publication)
        try await reply(publication){receiver.publishPresentation(publication.binding.identity,contextID:context.contextID,width:size.width,height:size.height,reply:$0)}
        _=try source.snapshot();publication.size=size;publication.published=true
        diagnostic(publication.binding.presentationID,"published","native_context_admitted")
    }
    private func resize(_ publication:Publication,size:CGSize,kind:String)async throws{
        guard publication.published,let renderer=publication.renderer,let context=publication.context else{return}
        guard publication.operation<9_007_199_254_740_991 else{throw DesktopPIPBinding.invalid()}
        let operation=publication.operation+1;let receiver=try receiver(publication);let fence=try context.makeFence()
        try await reply(publication){receiver.preparePresentation(publication.binding.identity,operationID:operation,kind:kind,
            contextID:context.contextID,width:size.width,height:size.height,fence:fence.payload,reply:$0)}
        _=try publication.pending.source.snapshot();try context.installFence(fence)
        try await renderer.configure(in:context.rootLayer,presentationSize:size,contentRevision:operation)
        _=try publication.pending.source.snapshot()
        try await reply(publication){receiver.completePresentation(publication.binding.identity,operationID:operation,reply:$0)}
        publication.operation=operation;publication.size=size
        if kind=="resize"{renderer.setFrameDeliverySuspended(false)}
    }
    private func reply(_ publication:Publication,send:@escaping(@escaping @Sendable(Error?)->Void)->Void)async throws{
        let box=DesktopPIPReply()
        try await withTaskCancellationHandler(operation:{
            try await withCheckedThrowingContinuation{(continuation:CheckedContinuation<Void,Error>) in
                box.install(continuation)
                let timeout=Task{[weak self,weak publication] in
                    do{try await Task.sleep(nanoseconds:15_000_000_000);try Task.checkCancellation();box.finish(DesktopFailure(code:"pip_receiver_timeout",message:"Native receiver did not acknowledge presentation"))
                        if let self,let publication{self.transport?.close(publication.peer)}
                    }catch{}
                }
                box.timeout=timeout
                send{error in box.finish(error)}
            }
        },onCancel:{box.finish(CancellationError())})
        _=try receiver(publication)
    }
    private func withdraw(_ publication:Publication,error:Error){
        guard !publication.disposed else{return}
        diagnostic(publication.binding.presentationID,"failed",(error as? DesktopFailure)?.code ?? "native_presentation_failed")
        if let transport,let receiver=try? transport.receiver(for:publication.peer,expectedPID:publication.binding.receiverPID){
            receiver.invalidatePresentation(publication.binding.identity,reason:"withdrawn",reply:{_ in})
        }
        cancel(publication.binding.bootstrapID)
    }
    private func dispose(_ publication:Publication){
        guard !publication.disposed else{return};publication.disposed=true;publication.serial?.cancel();publication.ending?.cancel();publication.completion?.cancel()
        let renderer=publication.renderer;publication.renderer=nil;renderer?.setFrameDeliverySuspended(true)
        publication.context?.invalidate();publication.context=nil
        let id=UUID()
        teardowns[id]=Task{[weak self] in await renderer?.stop();self?.teardowns.removeValue(forKey:id)}
    }
    private func disconnected(_ peer:NSXPCConnection){
        channels=channels.filter{$0.value !== peer};established.remove(ObjectIdentifier(peer))
        for (id,entry) in pending where entry.connection === peer {
            if let publication=publications[id],publication.endingSource{forgetDesired(publication)}
            cancel(id)
        }
        diagnostic("","disconnected","native_channel_closed")
    }
    private static func remaining(_ deadline:UInt64)throws->UInt64{
        var scale=mach_timebase_info_data_t()
        guard mach_timebase_info(&scale)==KERN_SUCCESS,scale.numer>0,scale.denom>0 else{throw DesktopPIPBinding.invalid()}
        let now=mach_continuous_time();guard deadline>now else{return 0}
        let wide=(deadline-now).multipliedFullWidth(by:UInt64(scale.numer))
        guard wide.high<UInt64(scale.denom) else{throw DesktopPIPBinding.invalid()}
        let value=UInt64(scale.denom).dividingFullWidth(wide).quotient
        guard value<=600_000_000_000 else{throw DesktopPIPBinding.invalid()};return value
    }
}

private final class DesktopPIPReply:@unchecked Sendable{
    private let lock=NSLock();private var continuation:CheckedContinuation<Void,Error>?;private var finished=false;private var result:Error?
    var timeout:Task<Void,Never>?
    func install(_ value:CheckedContinuation<Void,Error>){lock.lock();if finished{let error=result;lock.unlock();if let error{value.resume(throwing:error)}else{value.resume()};return};continuation=value;lock.unlock()}
    func finish(_ error:Error?){lock.lock();guard !finished else{lock.unlock();return};finished=true;result=error;let current=continuation;continuation=nil;lock.unlock();timeout?.cancel();if let error{current?.resume(throwing:error)}else{current?.resume()}}
}

private final class DesktopPIPPublisherEndpoint:NSObject,OCTPIPPublisher{
    private weak var owner:DesktopAppPIPPublisher?
    private let peer:NSXPCConnection
    init(owner:DesktopAppPIPPublisher,peer:NSXPCConnection){self.owner=owner;self.peer=peer}
    private func invoke(_ reply:@escaping(Error?)->Void,_ body:@escaping @MainActor(DesktopAppPIPPublisher)throws->Void){
        Task{@MainActor in do{guard let owner else{throw DesktopPIPBinding.invalid()};try body(owner);reply(nil)}catch{reply(error)}}
    }
    func connect(reply:@escaping(Error?)->Void){invoke(reply){try $0.connect(self.peer)}}
    func beginSourceAuthentication(_ bindingBytes:Data,receiverChallenge:Data,reply:@escaping(Data?,Data?,Error?)->Void){
        Task{@MainActor in do{guard let owner else{throw DesktopPIPBinding.invalid()};let (challenge,proof)=try owner.begin(bindingBytes,challenge:receiverChallenge,peer:peer);reply(challenge,proof,nil)}catch{owner?.authenticationFailed(peer,id:nil);reply(nil,nil,error)}}
    }
    func finishSourceAuthentication(_ bootstrapID:String,receiverProof:Data,reply:@escaping(Error?)->Void){invoke(reply){owner in do{try owner.finish(bootstrapID,proof:receiverProof,peer:self.peer)}catch{owner.authenticationFailed(self.peer,id:bootstrapID);throw error}}}
    func activateSource(_ bootstrapID:String,reply:@escaping(Error?)->Void){invoke(reply){try $0.activate(bootstrapID,peer:self.peer)}}
    func cancelSource(_ bootstrapID:String,reply:@escaping(Error?)->Void){invoke(reply){try $0.peerCancel(bootstrapID,peer:self.peer)}}
    func didEndPresentation(_ identity:[AnyHashable:Any],reply:@escaping(Error?)->Void){invoke(reply){guard let value=identity as? [String:Any] else{throw DesktopPIPBinding.invalid()};try $0.ended(value,peer:self.peer)}}
    func requestFocus(_ identity:[AnyHashable:Any],reply:@escaping(Error?)->Void){invoke(reply){guard let value=identity as? [String:Any] else{throw DesktopPIPBinding.invalid()};try $0.focus(value,peer:self.peer)}}
    func setMaxDisplaySize(_ size:Double,identity:[AnyHashable:Any],reply:@escaping(Error?)->Void){invoke(reply){guard let value=identity as? [String:Any] else{throw DesktopPIPBinding.invalid()};try $0.maximum(size,identity:value,peer:self.peer)}}
}
