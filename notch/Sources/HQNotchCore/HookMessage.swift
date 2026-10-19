import Foundation

/// One line from `notch/hook/hq_notch_hook.py` (protocol v1). See `build_message()` there.
public struct HookMessage: Decodable, Sendable {
    public struct Tool: Decodable, Sendable, Equatable {
        public var tool: String
        public var target: String
        public init(tool: String, target: String) { self.tool = tool; self.target = target }
    }

    public var v: Int
    public var event: String
    public var sessionId: String
    public var cwd: String
    public var transcriptPath: String
    public var ppid: Int
    public var env: [String: String]
    public var tool: Tool?
    public var toolUseId: String?
    public var plus: Int?
    public var minus: Int?
    public var error: String?
    public var prompt: String?
    public var message: String?
    public var notificationType: String?
    public var source: String?
    public var toolInput: JSONValue?
    public var canAlways: Bool?
    public var question: Bool?
    public var wantsReply: Bool?
    /// After an edit tool ran: what it changed (see `edit_diff()` in the hook), for the diff view.
    public var diff: EditDiff?

    public init(event: String, sessionId: String, cwd: String = "", tool: Tool? = nil) {
        v = 1; self.event = event; self.sessionId = sessionId; self.cwd = cwd
        transcriptPath = ""; ppid = 0; env = [:]; self.tool = tool
    }

    enum CodingKeys: String, CodingKey {
        case v, event, sessionId, cwd, transcriptPath, ppid, env, tool, toolUseId, plus, minus,
             error, prompt, message, notificationType, source, toolInput, canAlways, question, wantsReply, diff
    }

    public init(from d: Decoder) throws {
        let c = try d.container(keyedBy: CodingKeys.self)
        v = (try? c.decode(Int.self, forKey: .v)) ?? 1
        event = try c.decode(String.self, forKey: .event)
        sessionId = (try? c.decode(String.self, forKey: .sessionId)) ?? ""
        cwd = (try? c.decode(String.self, forKey: .cwd)) ?? ""
        transcriptPath = (try? c.decode(String.self, forKey: .transcriptPath)) ?? ""
        ppid = (try? c.decode(Int.self, forKey: .ppid)) ?? 0
        env = (try? c.decode([String: String].self, forKey: .env)) ?? [:]
        tool = try? c.decode(Tool.self, forKey: .tool)
        toolUseId = try? c.decode(String.self, forKey: .toolUseId)
        plus = try? c.decode(Int.self, forKey: .plus)
        minus = try? c.decode(Int.self, forKey: .minus)
        error = try? c.decode(String.self, forKey: .error)
        prompt = try? c.decode(String.self, forKey: .prompt)
        message = try? c.decode(String.self, forKey: .message)
        notificationType = try? c.decode(String.self, forKey: .notificationType)
        source = try? c.decode(String.self, forKey: .source)
        toolInput = try? c.decode(JSONValue.self, forKey: .toolInput)
        canAlways = try? c.decode(Bool.self, forKey: .canAlways)
        question = try? c.decode(Bool.self, forKey: .question)
        wantsReply = try? c.decode(Bool.self, forKey: .wantsReply)
        diff = try? c.decode(EditDiff.self, forKey: .diff)
    }
}

/// The change an edit tool made: old/new text per edit (Write: old is empty), each side
/// already capped by the hook; `truncated` when the hook cut something.
public struct EditDiff: Decodable, Sendable, Equatable {
    public struct Edit: Decodable, Sendable, Equatable {
        public var old: String
        public var new: String
        public init(old: String, new: String) { self.old = old; self.new = new }
    }
    public var edits: [Edit]
    public var truncated: Bool
    public init(edits: [Edit], truncated: Bool = false) { self.edits = edits; self.truncated = truncated }

    enum CodingKeys: String, CodingKey { case edits, truncated }
    public init(from d: Decoder) throws {
        let c = try d.container(keyedBy: CodingKeys.self)
        edits = (try? c.decode([Edit].self, forKey: .edits)) ?? []
        truncated = (try? c.decode(Bool.self, forKey: .truncated)) ?? false
    }
}

/// Minimal JSON tree for tool inputs shown on permission / question cards.
public enum JSONValue: Codable, Sendable, Equatable {
    case string(String), number(Double), bool(Bool), array([JSONValue]), object([String: JSONValue]), null

    public init(from d: Decoder) throws {
        let c = try d.singleValueContainer()
        if c.decodeNil() { self = .null }
        else if let b = try? c.decode(Bool.self) { self = .bool(b) }
        else if let n = try? c.decode(Double.self) { self = .number(n) }
        else if let s = try? c.decode(String.self) { self = .string(s) }
        else if let a = try? c.decode([JSONValue].self) { self = .array(a) }
        else { self = .object(try c.decode([String: JSONValue].self)) }
    }

    public func encode(to e: Encoder) throws {
        var c = e.singleValueContainer()
        switch self {
        case .string(let s): try c.encode(s)
        case .number(let n): try c.encode(n)
        case .bool(let b): try c.encode(b)
        case .array(let a): try c.encode(a)
        case .object(let o): try c.encode(o)
        case .null: try c.encodeNil()
        }
    }

    public subscript(key: String) -> JSONValue? {
        if case .object(let o) = self { return o[key] }
        return nil
    }
    public var string: String? { if case .string(let s) = self { return s }; return nil }
    public var bool: Bool? { if case .bool(let b) = self { return b }; return nil }
    public var array: [JSONValue]? { if case .array(let a) = self { return a }; return nil }
}

/// A question from an AskUserQuestion permission request.
public struct Question: Equatable, Sendable, Identifiable {
    public var id: String { text }
    public var text: String
    public var header: String
    public var options: [(label: String, detail: String)]
    public var multiSelect: Bool

    public static func == (a: Question, b: Question) -> Bool {
        a.text == b.text && a.multiSelect == b.multiSelect && a.options.map(\.label) == b.options.map(\.label)
    }

    public static func parse(_ input: JSONValue?) -> [Question] {
        (input?["questions"]?.array ?? []).compactMap { q in
            guard let text = q["question"]?.string else { return nil }
            let opts = (q["options"]?.array ?? []).compactMap { o -> (String, String)? in
                guard let l = o["label"]?.string else { return nil }
                return (l, o["description"]?.string ?? "")
            }
            return Question(text: text, header: q["header"]?.string ?? "", options: opts,
                            multiSelect: q["multiSelect"]?.bool ?? false)
        }
    }
}
