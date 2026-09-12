public func pcsuite_log_init() {
    __swift_bridge__$pcsuite_log_init()
}
public func pcsuite_abi_version() -> UInt32 {
    __swift_bridge__$pcsuite_abi_version()
}
public func pcsuite_set_identity<GenericIntoRustString: IntoRustString>(_ open_id: GenericIntoRustString, _ pc_mac: GenericIntoRustString, _ account: GenericIntoRustString, _ device_name: GenericIntoRustString) {
    __swift_bridge__$pcsuite_set_identity({ let rustString = open_id.intoRustString(); rustString.isOwned = false; return rustString.ptr }(), { let rustString = pc_mac.intoRustString(); rustString.isOwned = false; return rustString.ptr }(), { let rustString = account.intoRustString(); rustString.isOwned = false; return rustString.ptr }(), { let rustString = device_name.intoRustString(); rustString.isOwned = false; return rustString.ptr }())
}
public func pcsuite_set_seed<GenericIntoRustString: IntoRustString>(_ phone_ip: GenericIntoRustString, _ seed: GenericIntoRustString) {
    __swift_bridge__$pcsuite_set_seed({ let rustString = phone_ip.intoRustString(); rustString.isOwned = false; return rustString.ptr }(), { let rustString = seed.intoRustString(); rustString.isOwned = false; return rustString.ptr }())
}
public func pcsuite_set_clip_id<GenericIntoRustString: IntoRustString>(_ clip_id: GenericIntoRustString) {
    __swift_bridge__$pcsuite_set_clip_id({ let rustString = clip_id.intoRustString(); rustString.isOwned = false; return rustString.ptr }())
}
public func pcsuite_connect_usb() throws -> PcSession {
    try { let val = __swift_bridge__$pcsuite_connect_usb(); if val.is_ok { return PcSession(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func pcsuite_usb_probe() -> RustString {
    RustString(ptr: __swift_bridge__$pcsuite_usb_probe())
}
public func pcsuite_connect_lan<GenericIntoRustString: IntoRustString>(_ phone_ip: GenericIntoRustString, _ remote: Bool) throws -> PcSession {
    try { let val = __swift_bridge__$pcsuite_connect_lan({ let rustString = phone_ip.intoRustString(); rustString.isOwned = false; return rustString.ptr }(), remote); if val.is_ok { return PcSession(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func pcsuite_connect_lan_token<GenericIntoRustString: IntoRustString>(_ phone_ip: GenericIntoRustString, _ token: GenericIntoRustString) throws -> PcSession {
    try { let val = __swift_bridge__$pcsuite_connect_lan_token({ let rustString = phone_ip.intoRustString(); rustString.isOwned = false; return rustString.ptr }(), { let rustString = token.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return PcSession(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func pcsuite_cancel_connect() {
    __swift_bridge__$pcsuite_cancel_connect()
}
public func pcsuite_pair_begin<GenericIntoRustString: IntoRustString>(_ lip: GenericIntoRustString) -> PcPairing {
    PcPairing(ptr: __swift_bridge__$pcsuite_pair_begin({ let rustString = lip.intoRustString(); rustString.isOwned = false; return rustString.ptr }()))
}
public func pcsuite_presence_start() throws -> () {
    try { let val = __swift_bridge__$pcsuite_presence_start(); if val != nil { throw RustString(ptr: val!) } else { return } }()
}
public func pcsuite_presence_stop() {
    __swift_bridge__$pcsuite_presence_stop()
}
public func pcsuite_share_recv_start<GenericIntoRustString: IntoRustString>(_ save_dir: GenericIntoRustString) throws -> () {
    try { let val = __swift_bridge__$pcsuite_share_recv_start({ let rustString = save_dir.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val != nil { throw RustString(ptr: val!) } else { return } }()
}
public func pcsuite_share_recv_next_event() -> RustString {
    RustString(ptr: __swift_bridge__$pcsuite_share_recv_next_event())
}
public func pcsuite_share_recv_stop() {
    __swift_bridge__$pcsuite_share_recv_stop()
}
public func pcsuite_set_mode<GenericIntoRustString: IntoRustString>(_ mode: GenericIntoRustString) {
    __swift_bridge__$pcsuite_set_mode({ let rustString = mode.intoRustString(); rustString.isOwned = false; return rustString.ptr }())
}
public func pcsuite_mode() -> RustString {
    RustString(ptr: __swift_bridge__$pcsuite_mode())
}
public func pcsuite_cloud_set_account<GenericIntoRustString: IntoRustString>(_ open_id: GenericIntoRustString, _ token: GenericIntoRustString, _ country_code: GenericIntoRustString) {
    __swift_bridge__$pcsuite_cloud_set_account({ let rustString = open_id.intoRustString(); rustString.isOwned = false; return rustString.ptr }(), { let rustString = token.intoRustString(); rustString.isOwned = false; return rustString.ptr }(), { let rustString = country_code.intoRustString(); rustString.isOwned = false; return rustString.ptr }())
}
public func pcsuite_cloud_device_id() -> RustString {
    RustString(ptr: __swift_bridge__$pcsuite_cloud_device_id())
}
public func pcsuite_cloud_clip_pc_id() -> RustString {
    RustString(ptr: __swift_bridge__$pcsuite_cloud_clip_pc_id())
}
public func pcsuite_cloud_register() throws -> RustString {
    try { let val = __swift_bridge__$pcsuite_cloud_register(); if val.is_ok { return RustString(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func pcsuite_cloud_devices() throws -> RustString {
    try { let val = __swift_bridge__$pcsuite_cloud_devices(); if val.is_ok { return RustString(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func pcsuite_cloud_unregister() throws -> RustString {
    try { let val = __swift_bridge__$pcsuite_cloud_unregister(); if val.is_ok { return RustString(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
}
public func pcsuite_cloud_recv_start<GenericIntoRustString: IntoRustString>(_ save_dir: GenericIntoRustString, _ interval_secs: Double) throws -> () {
    try { let val = __swift_bridge__$pcsuite_cloud_recv_start({ let rustString = save_dir.intoRustString(); rustString.isOwned = false; return rustString.ptr }(), interval_secs); if val != nil { throw RustString(ptr: val!) } else { return } }()
}
public func pcsuite_cloud_recv_next_event() -> RustString {
    RustString(ptr: __swift_bridge__$pcsuite_cloud_recv_next_event())
}
public func pcsuite_cloud_recv_poll_now() {
    __swift_bridge__$pcsuite_cloud_recv_poll_now()
}
public func pcsuite_cloud_recv_stop() {
    __swift_bridge__$pcsuite_cloud_recv_stop()
}
public func pcsuite_cloud_presence_start<GenericIntoRustString: IntoRustString>(_ phone_ip: GenericIntoRustString, _ remote: Bool) -> PcCloudPresence {
    PcCloudPresence(ptr: __swift_bridge__$pcsuite_cloud_presence_start({ let rustString = phone_ip.intoRustString(); rustString.isOwned = false; return rustString.ptr }(), remote))
}

public class PcScreen: PcScreenRefMut {
    var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$PcScreen$_free(ptr)
        }
    }
}
public class PcScreenRefMut: PcScreenRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class PcScreenRef {
    var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension PcScreenRef {
    public func next_frame() -> RustVec<UInt8> {
        RustVec(ptr: __swift_bridge__$PcScreen$next_frame(ptr))
    }

    public func stop() {
        __swift_bridge__$PcScreen$stop(ptr)
    }

    public func next_privacy_event() -> RustString {
        RustString(ptr: __swift_bridge__$PcScreen$next_privacy_event(ptr))
    }

    public func next_audio_frame() -> RustVec<UInt8> {
        RustVec(ptr: __swift_bridge__$PcScreen$next_audio_frame(ptr))
    }

    public func next_input_cursor() -> RustString {
        RustString(ptr: __swift_bridge__$PcScreen$next_input_cursor(ptr))
    }
}
extension PcScreen: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_PcScreen$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_PcScreen$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: PcScreen) {
        __swift_bridge__$Vec_PcScreen$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_PcScreen$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (PcScreen(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<PcScreenRef> {
        let pointer = __swift_bridge__$Vec_PcScreen$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return PcScreenRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<PcScreenRefMut> {
        let pointer = __swift_bridge__$Vec_PcScreen$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return PcScreenRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<PcScreenRef> {
        UnsafePointer<PcScreenRef>(OpaquePointer(__swift_bridge__$Vec_PcScreen$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_PcScreen$len(vecPtr)
    }
}


public class PcSession: PcSessionRefMut {
    var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$PcSession$_free(ptr)
        }
    }
}
public class PcSessionRefMut: PcSessionRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class PcSessionRef {
    var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension PcSessionRef {
    public func start_screen(_ max_size: Int64, _ bit_rate: Int64, _ frame_rate: Int64, _ audio: Bool) throws -> PcScreen {
        try { let val = __swift_bridge__$PcSession$start_screen(ptr, max_size, bit_rate, frame_rate, audio); if val.is_ok { return PcScreen(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
    }

    public func enable_clipboard(_ recv: Bool, _ send: Bool) throws -> () {
        try { let val = __swift_bridge__$PcSession$enable_clipboard(ptr, recv, send); if val != nil { throw RustString(ptr: val!) } else { return } }()
    }

    public func stop_clipboard() {
        __swift_bridge__$PcSession$stop_clipboard(ptr)
    }

    public func enable_verify() {
        __swift_bridge__$PcSession$enable_verify(ptr)
    }

    public func next_verify_code() -> RustString {
        RustString(ptr: __swift_bridge__$PcSession$next_verify_code(ptr))
    }

    public func stop_verify() {
        __swift_bridge__$PcSession$stop_verify(ptr)
    }

    public func enable_notify() {
        __swift_bridge__$PcSession$enable_notify(ptr)
    }

    public func next_notification() -> RustString {
        RustString(ptr: __swift_bridge__$PcSession$next_notification(ptr))
    }

    public func stop_notify() {
        __swift_bridge__$PcSession$stop_notify(ptr)
    }

    public func next_connect_center_request() -> RustString {
        RustString(ptr: __swift_bridge__$PcSession$next_connect_center_request(ptr))
    }

    public func stop_connect_center() {
        __swift_bridge__$PcSession$stop_connect_center(ptr)
    }

    public func reply_connect_center<GenericIntoRustString: IntoRustString>(_ name: GenericIntoRustString, _ msg_id: GenericIntoRustString, _ code: Int64, _ reason: GenericIntoRustString) {
        __swift_bridge__$PcSession$reply_connect_center(ptr, { let rustString = name.intoRustString(); rustString.isOwned = false; return rustString.ptr }(), { let rustString = msg_id.intoRustString(); rustString.isOwned = false; return rustString.ptr }(), code, { let rustString = reason.intoRustString(); rustString.isOwned = false; return rustString.ptr }())
    }

    public func push_files<GenericIntoRustString: IntoRustString>(_ paths: RustVec<GenericIntoRustString>, _ save_dir: GenericIntoRustString) throws -> RustString {
        try { let val = __swift_bridge__$PcSession$push_files(ptr, { let val = paths; val.isOwned = false; return val.ptr }(), { let rustString = save_dir.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val.is_ok { return RustString(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
    }

    public func enable_file_transfer<GenericIntoRustString: IntoRustString>(_ save_dir: GenericIntoRustString) throws -> () {
        try { let val = __swift_bridge__$PcSession$enable_file_transfer(ptr, { let rustString = save_dir.intoRustString(); rustString.isOwned = false; return rustString.ptr }()); if val != nil { throw RustString(ptr: val!) } else { return } }()
    }

    public func next_file_transfer_event() -> RustString {
        RustString(ptr: __swift_bridge__$PcSession$next_file_transfer_event(ptr))
    }

    public func stop_file_transfer() {
        __swift_bridge__$PcSession$stop_file_transfer(ptr)
    }

    public func device_info() throws -> RustString {
        try { let val = __swift_bridge__$PcSession$device_info(ptr); if val.is_ok { return RustString(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
    }

    public func wait_disconnect() -> RustString {
        RustString(ptr: __swift_bridge__$PcSession$wait_disconnect(ptr))
    }

    public func stop_watch() {
        __swift_bridge__$PcSession$stop_watch(ptr)
    }

    public func mouse(_ action: UInt8, _ button: UInt8, _ x: Int64, _ y: Int64, _ w: Int64, _ h: Int64) -> Bool {
        __swift_bridge__$PcSession$mouse(ptr, action, button, x, y, w, h)
    }

    public func scroll(_ vscroll: Int64, _ x: Int64, _ y: Int64, _ w: Int64, _ h: Int64) -> Bool {
        __swift_bridge__$PcSession$scroll(ptr, vscroll, x, y, w, h)
    }

    public func text<GenericIntoRustString: IntoRustString>(_ s: GenericIntoRustString) -> Bool {
        __swift_bridge__$PcSession$text(ptr, { let rustString = s.intoRustString(); rustString.isOwned = false; return rustString.ptr }())
    }

    public func delete_surrounding(_ before: Int64, _ after: Int64) -> Bool {
        __swift_bridge__$PcSession$delete_surrounding(ptr, before, after)
    }

    public func tap(_ x: Int64, _ y: Int64, _ w: Int64, _ h: Int64) -> Bool {
        __swift_bridge__$PcSession$tap(ptr, x, y, w, h)
    }

    public func key(_ keycode: Int64) -> Bool {
        __swift_bridge__$PcSession$key(ptr, keycode)
    }

    public func set_audio_to_pc(_ to_pc: Bool) -> Bool {
        __swift_bridge__$PcSession$set_audio_to_pc(ptr, to_pc)
    }
}
extension PcSession: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_PcSession$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_PcSession$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: PcSession) {
        __swift_bridge__$Vec_PcSession$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_PcSession$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (PcSession(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<PcSessionRef> {
        let pointer = __swift_bridge__$Vec_PcSession$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return PcSessionRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<PcSessionRefMut> {
        let pointer = __swift_bridge__$Vec_PcSession$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return PcSessionRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<PcSessionRef> {
        UnsafePointer<PcSessionRef>(OpaquePointer(__swift_bridge__$Vec_PcSession$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_PcSession$len(vecPtr)
    }
}


public class PcCloudPresence: PcCloudPresenceRefMut {
    var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$PcCloudPresence$_free(ptr)
        }
    }
}
public class PcCloudPresenceRefMut: PcCloudPresenceRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class PcCloudPresenceRef {
    var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension PcCloudPresenceRef {
    public func status() -> RustString {
        RustString(ptr: __swift_bridge__$PcCloudPresence$status(ptr))
    }

    public func take_connect_request() -> RustString {
        RustString(ptr: __swift_bridge__$PcCloudPresence$take_connect_request(ptr))
    }

    public func report_connect_result<GenericIntoRustString: IntoRustString>(_ ret_code: Int64, _ ret_msg: GenericIntoRustString) {
        __swift_bridge__$PcCloudPresence$report_connect_result(ptr, ret_code, { let rustString = ret_msg.intoRustString(); rustString.isOwned = false; return rustString.ptr }())
    }

    public func report_session_ended() {
        __swift_bridge__$PcCloudPresence$report_session_ended(ptr)
    }

    public func upgrade_for_connect() -> RustString {
        RustString(ptr: __swift_bridge__$PcCloudPresence$upgrade_for_connect(ptr))
    }

    public func phone_ip() -> RustString {
        RustString(ptr: __swift_bridge__$PcCloudPresence$phone_ip(ptr))
    }

    public func stop() {
        __swift_bridge__$PcCloudPresence$stop(ptr)
    }
}
extension PcCloudPresence: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_PcCloudPresence$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_PcCloudPresence$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: PcCloudPresence) {
        __swift_bridge__$Vec_PcCloudPresence$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_PcCloudPresence$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (PcCloudPresence(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<PcCloudPresenceRef> {
        let pointer = __swift_bridge__$Vec_PcCloudPresence$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return PcCloudPresenceRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<PcCloudPresenceRefMut> {
        let pointer = __swift_bridge__$Vec_PcCloudPresence$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return PcCloudPresenceRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<PcCloudPresenceRef> {
        UnsafePointer<PcCloudPresenceRef>(OpaquePointer(__swift_bridge__$Vec_PcCloudPresence$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_PcCloudPresence$len(vecPtr)
    }
}


public class PcPairing: PcPairingRefMut {
    var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$PcPairing$_free(ptr)
        }
    }
}
public class PcPairingRefMut: PcPairingRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class PcPairingRef {
    var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension PcPairingRef {
    public func qr_url() -> RustString {
        RustString(ptr: __swift_bridge__$PcPairing$qr_url(ptr))
    }

    public func lan_ip() -> RustString {
        RustString(ptr: __swift_bridge__$PcPairing$lan_ip(ptr))
    }

    public func wait_phone(_ timeout_ms: UInt32) throws -> PcPaired {
        try { let val = __swift_bridge__$PcPairing$wait_phone(ptr, timeout_ms); if val.is_ok { return PcPaired(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
    }

    public func cancel() {
        __swift_bridge__$PcPairing$cancel(ptr)
    }
}
extension PcPairing: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_PcPairing$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_PcPairing$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: PcPairing) {
        __swift_bridge__$Vec_PcPairing$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_PcPairing$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (PcPairing(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<PcPairingRef> {
        let pointer = __swift_bridge__$Vec_PcPairing$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return PcPairingRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<PcPairingRefMut> {
        let pointer = __swift_bridge__$Vec_PcPairing$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return PcPairingRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<PcPairingRef> {
        UnsafePointer<PcPairingRef>(OpaquePointer(__swift_bridge__$Vec_PcPairing$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_PcPairing$len(vecPtr)
    }
}


public class PcPaired: PcPairedRefMut {
    var isOwned: Bool = true

    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$PcPaired$_free(ptr)
        }
    }
}
public class PcPairedRefMut: PcPairedRef {
    public override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }
}
public class PcPairedRef {
    var ptr: UnsafeMutableRawPointer

    public init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }
}
extension PcPairedRef {
    public func phone_ip() -> RustString {
        RustString(ptr: __swift_bridge__$PcPaired$phone_ip(ptr))
    }

    public func ble_id() -> RustString {
        RustString(ptr: __swift_bridge__$PcPaired$ble_id(ptr))
    }

    public func device_name() -> RustString {
        RustString(ptr: __swift_bridge__$PcPaired$device_name(ptr))
    }

    public func vivo_account() -> RustString {
        RustString(ptr: __swift_bridge__$PcPaired$vivo_account(ptr))
    }

    public func device_type() -> RustString {
        RustString(ptr: __swift_bridge__$PcPaired$device_type(ptr))
    }

    public func connect() throws -> PcSession {
        try { let val = __swift_bridge__$PcPaired$connect(ptr); if val.is_ok { return PcSession(ptr: val.ok_or_err!) } else { throw RustString(ptr: val.ok_or_err!) } }()
    }
}
extension PcPaired: Vectorizable {
    public static func vecOfSelfNew() -> UnsafeMutableRawPointer {
        __swift_bridge__$Vec_PcPaired$new()
    }

    public static func vecOfSelfFree(vecPtr: UnsafeMutableRawPointer) {
        __swift_bridge__$Vec_PcPaired$drop(vecPtr)
    }

    public static func vecOfSelfPush(vecPtr: UnsafeMutableRawPointer, value: PcPaired) {
        __swift_bridge__$Vec_PcPaired$push(vecPtr, {value.isOwned = false; return value.ptr;}())
    }

    public static func vecOfSelfPop(vecPtr: UnsafeMutableRawPointer) -> Optional<Self> {
        let pointer = __swift_bridge__$Vec_PcPaired$pop(vecPtr)
        if pointer == nil {
            return nil
        } else {
            return (PcPaired(ptr: pointer!) as! Self)
        }
    }

    public static func vecOfSelfGet(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<PcPairedRef> {
        let pointer = __swift_bridge__$Vec_PcPaired$get(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return PcPairedRef(ptr: pointer!)
        }
    }

    public static func vecOfSelfGetMut(vecPtr: UnsafeMutableRawPointer, index: UInt) -> Optional<PcPairedRefMut> {
        let pointer = __swift_bridge__$Vec_PcPaired$get_mut(vecPtr, index)
        if pointer == nil {
            return nil
        } else {
            return PcPairedRefMut(ptr: pointer!)
        }
    }

    public static func vecOfSelfAsPtr(vecPtr: UnsafeMutableRawPointer) -> UnsafePointer<PcPairedRef> {
        UnsafePointer<PcPairedRef>(OpaquePointer(__swift_bridge__$Vec_PcPaired$as_ptr(vecPtr)))
    }

    public static func vecOfSelfLen(vecPtr: UnsafeMutableRawPointer) -> UInt {
        __swift_bridge__$Vec_PcPaired$len(vecPtr)
    }
}



