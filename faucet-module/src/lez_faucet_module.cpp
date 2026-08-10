#include "lez_faucet_module.h"

#include <algorithm>
#include <cstddef>
#include <optional>
#include <utility>
#include <vector>

namespace {

/// Balances are u128 on the wire, so they are u128 here. `__int128` is a
/// compiler extension rather than ISO C++, which `-Wpedantic` is correct to
/// point out — hence the narrowest possible suppression, at the one
/// declaration, instead of dropping the flag for the whole target. AppleClang
/// accepts this silently and GCC does not, so without the pragma the module
/// harness builds on a developer's macOS and fails on a Linux runner.
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wpedantic"
using U128 = unsigned __int128;
#pragma GCC diagnostic pop

/// Retained job records. Bounded and reapable — unlike request-key tombstones,
/// which are a separate table and are never evicted (API_LOCK §A1.7.4).
constexpr std::size_t kMaxRetainedJobs = 128;

/// Longest address accepted, after the `Public/` prefix is stripped.
///
/// This bound is load-bearing rather than cosmetic (API_LOCK §A1.3): the
/// base58 decoder behind the Rust parser subtracts with overflow on 133 or
/// more leading `1` characters, and a panic unwinding out of `extern "C"`
/// aborts LogosBasecamp. The Rust side enforces this too; the module does not
/// get to assume its caller is well behaved.
constexpr std::size_t kMaxAddressLen = 64;

/// `"Public/"`, the one accepted address prefix.
constexpr const char* kPublicPrefix = "Public/";
constexpr std::size_t kPublicPrefixLen = 7;

/// A request key is a lowercase RFC 4122 UUID and nothing else.
constexpr std::size_t kRequestKeyLen = 36;

/// Longest sequencer URL we will even look at.
constexpr std::size_t kMaxSequencerUrlLen = 2048;

/// Deepest JSON nesting accepted from the FFI.
///
/// The parser is recursive, so an adversarial payload of a million open braces
/// would otherwise exhaust the stack. Our own payloads nest three deep.
constexpr int kMaxJsonDepth = 32;

// ---- JSON ------------------------------------------------------------------
//
// A real parser, not a substring scanner. The previous implementation looked
// for `"ok"` anywhere in the document, so an error *message* that happened to
// contain `"ok":true` made a failed operation report success (API_LOCK
// §A1.7.3). Every value below is located structurally.

class Json {
public:
    enum class Kind { Null, Bool, Number, String, Array, Object };

    Json() = default;
    static Json boolean(bool value)
    {
        Json json;
        json.m_kind = Kind::Bool;
        json.m_bool = value;
        return json;
    }
    /// Numbers keep their original lexeme.
    ///
    /// Nothing in this module does arithmetic on a JSON number, and round
    /// tripping through `double` would silently corrupt any integer above 2^53.
    static Json number(std::string lexeme)
    {
        Json json;
        json.m_kind = Kind::Number;
        json.m_text = std::move(lexeme);
        return json;
    }
    static Json string(std::string value)
    {
        Json json;
        json.m_kind = Kind::String;
        json.m_text = std::move(value);
        return json;
    }
    static Json array()
    {
        Json json;
        json.m_kind = Kind::Array;
        return json;
    }
    static Json object()
    {
        Json json;
        json.m_kind = Kind::Object;
        return json;
    }

    bool isNull() const { return m_kind == Kind::Null; }
    bool isBool() const { return m_kind == Kind::Bool; }
    bool isString() const { return m_kind == Kind::String; }
    bool isObject() const { return m_kind == Kind::Object; }

    bool boolValue() const { return m_kind == Kind::Bool && m_bool; }
    /// The decoded text of a string, or the lexeme of a number.
    const std::string& text() const { return m_text; }

    /// Look up a member. Returns null for a non-object or a missing key, so a
    /// caller can chain `at("error").at("code")` without checking each step.
    const Json& at(const std::string& key) const
    {
        static const Json kNull;
        if (m_kind != Kind::Object) {
            return kNull;
        }
        for (const auto& member : m_members) {
            if (member.first == key) {
                return member.second;
            }
        }
        return kNull;
    }

    /// Insert or replace a member, preserving first-insertion order so that
    /// serialized output is deterministic and diffable.
    void set(const std::string& key, Json value)
    {
        for (auto& member : m_members) {
            if (member.first == key) {
                member.second = std::move(value);
                return;
            }
        }
        m_members.emplace_back(key, std::move(value));
    }

    void push(Json value) { m_elements.push_back(std::move(value)); }

    std::string dump() const
    {
        std::string out;
        write(out);
        return out;
    }

private:
    friend class JsonParser;

    void write(std::string& out) const;

    Kind m_kind = Kind::Null;
    bool m_bool = false;
    std::string m_text;
    std::vector<std::pair<std::string, Json>> m_members;
    std::vector<Json> m_elements;
};

void encodeJsonString(const std::string& value, std::string& out)
{
    out.push_back('"');
    for (const unsigned char character : value) {
        switch (character) {
        case '\\': out += "\\\\"; break;
        case '"': out += "\\\""; break;
        case '\n': out += "\\n"; break;
        case '\r': out += "\\r"; break;
        case '\t': out += "\\t"; break;
        case '\b': out += "\\b"; break;
        case '\f': out += "\\f"; break;
        default:
            if (character < 0x20) {
                static constexpr char digits[] = "0123456789abcdef";
                out += "\\u00";
                out.push_back(digits[(character >> 4) & 0xf]);
                out.push_back(digits[character & 0xf]);
            } else {
                out.push_back(static_cast<char>(character));
            }
        }
    }
    out.push_back('"');
}

void Json::write(std::string& out) const
{
    switch (m_kind) {
    case Kind::Null:
        out += "null";
        break;
    case Kind::Bool:
        out += m_bool ? "true" : "false";
        break;
    case Kind::Number:
        out += m_text;
        break;
    case Kind::String:
        encodeJsonString(m_text, out);
        break;
    case Kind::Array: {
        out.push_back('[');
        bool first = true;
        for (const Json& element : m_elements) {
            if (!first) {
                out.push_back(',');
            }
            first = false;
            element.write(out);
        }
        out.push_back(']');
        break;
    }
    case Kind::Object: {
        out.push_back('{');
        bool first = true;
        for (const auto& member : m_members) {
            if (!first) {
                out.push_back(',');
            }
            first = false;
            encodeJsonString(member.first, out);
            out.push_back(':');
            member.second.write(out);
        }
        out.push_back('}');
        break;
    }
    }
}

/// A small recursive-descent parser over the JSON subset this API emits.
///
/// It is deliberately strict: anything it cannot prove well-formed is rejected
/// rather than guessed at, because the alternative is deciding an operation
/// succeeded on the strength of a coincidence in an error message.
class JsonParser {
public:
    explicit JsonParser(const std::string& document)
        : m_document(document)
    {
    }

    std::optional<Json> parse()
    {
        skipWhitespace();
        std::optional<Json> value = parseValue(0);
        if (!value.has_value()) {
            return std::nullopt;
        }
        skipWhitespace();
        if (m_cursor != m_document.size()) {
            return std::nullopt;
        }
        return value;
    }

private:
    bool atEnd() const { return m_cursor >= m_document.size(); }
    char peek() const { return m_document[m_cursor]; }

    void skipWhitespace()
    {
        while (!atEnd()) {
            const char current = peek();
            if (current == ' ' || current == '\t' || current == '\n' || current == '\r') {
                ++m_cursor;
            } else {
                break;
            }
        }
    }

    bool consumeLiteral(const char* literal)
    {
        const std::size_t length = std::char_traits<char>::length(literal);
        if (m_document.compare(m_cursor, length, literal) != 0) {
            return false;
        }
        m_cursor += length;
        return true;
    }

    std::optional<Json> parseValue(int depth)
    {
        if (depth > kMaxJsonDepth || atEnd()) {
            return std::nullopt;
        }
        switch (peek()) {
        case '{': return parseObject(depth);
        case '[': return parseArray(depth);
        case '"': {
            std::string value;
            if (!parseString(value)) {
                return std::nullopt;
            }
            return Json::string(std::move(value));
        }
        case 't':
            return consumeLiteral("true") ? std::optional<Json>(Json::boolean(true)) : std::nullopt;
        case 'f':
            return consumeLiteral("false") ? std::optional<Json>(Json::boolean(false)) : std::nullopt;
        case 'n':
            return consumeLiteral("null") ? std::optional<Json>(Json()) : std::nullopt;
        default:
            return parseNumber();
        }
    }

    std::optional<Json> parseObject(int depth)
    {
        Json object = Json::object();
        ++m_cursor; // '{'
        skipWhitespace();
        if (!atEnd() && peek() == '}') {
            ++m_cursor;
            return object;
        }
        for (;;) {
            skipWhitespace();
            if (atEnd() || peek() != '"') {
                return std::nullopt;
            }
            std::string key;
            if (!parseString(key)) {
                return std::nullopt;
            }
            skipWhitespace();
            if (atEnd() || peek() != ':') {
                return std::nullopt;
            }
            ++m_cursor;
            skipWhitespace();
            std::optional<Json> value = parseValue(depth + 1);
            if (!value.has_value()) {
                return std::nullopt;
            }
            object.set(key, std::move(*value));
            skipWhitespace();
            if (atEnd()) {
                return std::nullopt;
            }
            if (peek() == ',') {
                ++m_cursor;
                continue;
            }
            if (peek() == '}') {
                ++m_cursor;
                return object;
            }
            return std::nullopt;
        }
    }

    std::optional<Json> parseArray(int depth)
    {
        Json array = Json::array();
        ++m_cursor; // '['
        skipWhitespace();
        if (!atEnd() && peek() == ']') {
            ++m_cursor;
            return array;
        }
        for (;;) {
            skipWhitespace();
            std::optional<Json> value = parseValue(depth + 1);
            if (!value.has_value()) {
                return std::nullopt;
            }
            array.push(std::move(*value));
            skipWhitespace();
            if (atEnd()) {
                return std::nullopt;
            }
            if (peek() == ',') {
                ++m_cursor;
                continue;
            }
            if (peek() == ']') {
                ++m_cursor;
                return array;
            }
            return std::nullopt;
        }
    }

    /// Decode one JSON string, escapes and all.
    ///
    /// Escapes are the whole reason a scanner cannot be trusted: `"\"ok\":true"`
    /// inside a message is text, not structure, and only a decoder that walks
    /// escapes can tell the two apart.
    bool parseString(std::string& out)
    {
        out.clear();
        ++m_cursor; // opening quote
        while (!atEnd()) {
            const unsigned char current = static_cast<unsigned char>(m_document[m_cursor]);
            if (current == '"') {
                ++m_cursor;
                return true;
            }
            if (current < 0x20) {
                return false; // raw control characters are not legal in a string
            }
            if (current != '\\') {
                out.push_back(static_cast<char>(current));
                ++m_cursor;
                continue;
            }
            ++m_cursor;
            if (atEnd()) {
                return false;
            }
            const char escape = m_document[m_cursor++];
            switch (escape) {
            case '"': out.push_back('"'); break;
            case '\\': out.push_back('\\'); break;
            case '/': out.push_back('/'); break;
            case 'b': out.push_back('\b'); break;
            case 'f': out.push_back('\f'); break;
            case 'n': out.push_back('\n'); break;
            case 'r': out.push_back('\r'); break;
            case 't': out.push_back('\t'); break;
            case 'u': {
                uint32_t code = 0;
                if (!parseHex4(code)) {
                    return false;
                }
                // A high surrogate is only meaningful paired with its low half.
                if (code >= 0xD800 && code <= 0xDBFF && m_document.compare(m_cursor, 2, "\\u") == 0) {
                    const std::size_t mark = m_cursor;
                    m_cursor += 2;
                    uint32_t low = 0;
                    if (parseHex4(low) && low >= 0xDC00 && low <= 0xDFFF) {
                        code = 0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00);
                    } else {
                        m_cursor = mark;
                    }
                }
                appendUtf8(code, out);
                break;
            }
            default:
                return false;
            }
        }
        return false;
    }

    bool parseHex4(uint32_t& out)
    {
        if (m_cursor + 4 > m_document.size()) {
            return false;
        }
        out = 0;
        for (int index = 0; index < 4; ++index) {
            const char digit = m_document[m_cursor + static_cast<std::size_t>(index)];
            uint32_t value = 0;
            if (digit >= '0' && digit <= '9') {
                value = static_cast<uint32_t>(digit - '0');
            } else if (digit >= 'a' && digit <= 'f') {
                value = static_cast<uint32_t>(digit - 'a') + 10;
            } else if (digit >= 'A' && digit <= 'F') {
                value = static_cast<uint32_t>(digit - 'A') + 10;
            } else {
                return false;
            }
            out = (out << 4) | value;
        }
        m_cursor += 4;
        return true;
    }

    static void appendUtf8(uint32_t code, std::string& out)
    {
        // A lone surrogate cannot be encoded; substitute the replacement
        // character rather than emitting invalid UTF-8 into Basecamp.
        if (code >= 0xD800 && code <= 0xDFFF) {
            code = 0xFFFD;
        }
        if (code < 0x80) {
            out.push_back(static_cast<char>(code));
        } else if (code < 0x800) {
            out.push_back(static_cast<char>(0xC0 | (code >> 6)));
            out.push_back(static_cast<char>(0x80 | (code & 0x3F)));
        } else if (code < 0x10000) {
            out.push_back(static_cast<char>(0xE0 | (code >> 12)));
            out.push_back(static_cast<char>(0x80 | ((code >> 6) & 0x3F)));
            out.push_back(static_cast<char>(0x80 | (code & 0x3F)));
        } else {
            out.push_back(static_cast<char>(0xF0 | (code >> 18)));
            out.push_back(static_cast<char>(0x80 | ((code >> 12) & 0x3F)));
            out.push_back(static_cast<char>(0x80 | ((code >> 6) & 0x3F)));
            out.push_back(static_cast<char>(0x80 | (code & 0x3F)));
        }
    }

    std::optional<Json> parseNumber()
    {
        const std::size_t start = m_cursor;
        if (!atEnd() && peek() == '-') {
            ++m_cursor;
        }
        const std::size_t digitsStart = m_cursor;
        while (!atEnd() && peek() >= '0' && peek() <= '9') {
            ++m_cursor;
        }
        if (m_cursor == digitsStart) {
            return std::nullopt;
        }
        if (!atEnd() && peek() == '.') {
            ++m_cursor;
            const std::size_t fractionStart = m_cursor;
            while (!atEnd() && peek() >= '0' && peek() <= '9') {
                ++m_cursor;
            }
            if (m_cursor == fractionStart) {
                return std::nullopt;
            }
        }
        if (!atEnd() && (peek() == 'e' || peek() == 'E')) {
            ++m_cursor;
            if (!atEnd() && (peek() == '+' || peek() == '-')) {
                ++m_cursor;
            }
            const std::size_t exponentStart = m_cursor;
            while (!atEnd() && peek() >= '0' && peek() <= '9') {
                ++m_cursor;
            }
            if (m_cursor == exponentStart) {
                return std::nullopt;
            }
        }
        return Json::number(m_document.substr(start, m_cursor - start));
    }

    const std::string& m_document;
    std::size_t m_cursor = 0;
};

std::optional<Json> parseJson(const std::string& document)
{
    return JsonParser(document).parse();
}

// ---- exact decimals --------------------------------------------------------

/// Parse a canonical unsigned decimal string into a `u128`.
///
/// Balances cross this boundary as strings precisely so that values above
/// JavaScript's safe-integer range stay exact; parsing one into a `double`
/// anywhere in the chain would quietly round a user's balance.
bool parseU128(const std::string& raw, U128& output)
{
    if (raw.empty()) {
        return false;
    }
    U128 parsed = 0;
    constexpr U128 max = ~static_cast<U128>(0);
    for (const char character : raw) {
        if (character < '0' || character > '9') {
            return false;
        }
        const unsigned digit = static_cast<unsigned>(character - '0');
        if (parsed > (max - digit) / 10) {
            return false;
        }
        parsed = parsed * 10 + digit;
    }
    output = parsed;
    return true;
}

std::string u128String(U128 value)
{
    if (value == 0) {
        return "0";
    }
    std::string out;
    while (value != 0) {
        out.push_back(static_cast<char>('0' + static_cast<unsigned>(value % 10)));
        value /= 10;
    }
    std::reverse(out.begin(), out.end());
    return out;
}

/// Re-render one decimal field in canonical form, or report it malformed.
///
/// Rust already emits canonical decimals; this re-derives them rather than
/// trusting the claim, so a leading zero or a stray sign fails loudly at the
/// boundary instead of reaching QML as a plausible-looking balance.
bool normalizeDecimalField(Json& object, const std::string& key, bool required)
{
    const Json& field = object.at(key);
    if (field.isNull()) {
        return !required;
    }
    if (!field.isString()) {
        return false;
    }
    U128 value = 0;
    if (!parseU128(field.text(), value)) {
        return false;
    }
    object.set(key, Json::string(u128String(value)));
    return true;
}

// ---- envelopes -------------------------------------------------------------

Json errorObject(const std::string& code, const std::string& message)
{
    Json error = Json::object();
    error.set("code", Json::string(code));
    error.set("message", Json::string(message));
    error.set("retryable", Json::boolean(false));
    return error;
}

std::string errorEnvelope(const std::string& code, const std::string& message)
{
    Json envelope = Json::object();
    envelope.set("ok", Json::boolean(false));
    envelope.set("error", errorObject(code, message));
    return envelope.dump();
}

/// The one place an FFI envelope is judged successful.
///
/// It reads the *structural* `ok` member of the top-level object. Nothing here
/// looks at message text, and nothing here may be replaced by something that
/// does.
bool envelopeSucceeded(const Json& envelope)
{
    const Json& ok = envelope.at("ok");
    return ok.isBool() && ok.boolValue();
}

/// Validate a user-supplied public address before it reaches the FFI.
///
/// The module does not trust QML (API_LOCK §A1.3). Rust validates too; both
/// checks are deliberate, because either layer may be called by something that
/// skipped the other.
bool addressIsWellFormed(const std::string& address, std::string& reason)
{
    // Trim the way the Rust parser does, so both layers judge the same value.
    const auto notSpace = [](unsigned char character) {
        return character != ' ' && character != '\t' && character != '\n' && character != '\r';
    };
    std::size_t begin = 0;
    while (begin < address.size() && !notSpace(static_cast<unsigned char>(address[begin]))) {
        ++begin;
    }
    std::size_t end = address.size();
    while (end > begin && !notSpace(static_cast<unsigned char>(address[end - 1]))) {
        --end;
    }
    const std::string trimmed = address.substr(begin, end - begin);

    if (trimmed.empty()) {
        reason = "Enter a public LEZ account address.";
        return false;
    }
    // An embedded NUL would be truncated by the C string this becomes, so what
    // Rust validated and what the caller typed would differ. Control characters
    // mean the value was mangled in transit either way.
    for (const unsigned char character : trimmed) {
        if (character < 0x20 || character == 0x7F) {
            reason = "That address contains invalid characters.";
            return false;
        }
    }
    std::string bare = trimmed;
    const std::size_t slash = trimmed.find('/');
    if (slash != std::string::npos) {
        if (trimmed.compare(0, kPublicPrefixLen, kPublicPrefix) != 0 || slash != kPublicPrefixLen - 1) {
            reason = "Only a public LEZ account address can receive a faucet drop.";
            return false;
        }
        bare = trimmed.substr(kPublicPrefixLen);
    }
    if (bare.empty()) {
        reason = "Enter a public LEZ account address.";
        return false;
    }
    if (bare.size() > kMaxAddressLen) {
        reason = "That address is too long to be a LEZ account ID.";
        return false;
    }
    return true;
}

/// The canonical bare account id for an accepted address.
///
/// Used only to compare a replayed request key against its original recipient.
/// The authoritative normalization is still Rust's.
std::string bareAccountId(const std::string& address)
{
    std::string trimmed = address;
    const auto isSpace = [](char character) {
        return character == ' ' || character == '\t' || character == '\n' || character == '\r';
    };
    while (!trimmed.empty() && isSpace(trimmed.front())) {
        trimmed.erase(trimmed.begin());
    }
    while (!trimmed.empty() && isSpace(trimmed.back())) {
        trimmed.pop_back();
    }
    if (trimmed.compare(0, kPublicPrefixLen, kPublicPrefix) == 0) {
        return trimmed.substr(kPublicPrefixLen);
    }
    return trimmed;
}

/// A request key must be a lowercase RFC 4122 UUID (API_LOCK D6).
bool requestKeyIsWellFormed(const std::string& key)
{
    if (key.size() != kRequestKeyLen) {
        return false;
    }
    for (std::size_t index = 0; index < key.size(); ++index) {
        const char character = key[index];
        const bool ok = (index == 8 || index == 13 || index == 18 || index == 23)
            ? character == '-'
            : ((character >= '0' && character <= '9') || (character >= 'a' && character <= 'f'));
        if (!ok) {
            return false;
        }
    }
    return true;
}

bool sequencerUrlIsWellFormed(const std::string& url, std::string& reason)
{
    if (url.empty()) {
        reason = "A sequencer URL is required.";
        return false;
    }
    if (url.size() > kMaxSequencerUrlLen) {
        reason = "That sequencer URL is too long.";
        return false;
    }
    for (const unsigned char character : url) {
        if (character < 0x20 || character == 0x7F) {
            reason = "That sequencer URL contains invalid characters.";
            return false;
        }
    }
    return true;
}

/// Normalize the decimal fields of one result payload by operation.
///
/// A malformed decimal is a failure of the whole operation, not a field to be
/// passed through and hoped about: every one of these values is money.
bool normalizeResult(const std::string& operation, Json& result)
{
    if (!result.isObject()) {
        return false;
    }
    if (operation == "faucet_info") {
        return normalizeDecimalField(result, "prize_amount", true)
            && normalizeDecimalField(result, "pool_balance", true)
            && normalizeDecimalField(result, "claims_remaining", true);
    }
    if (operation == "inspect_recipient") {
        return normalizeDecimalField(result, "balance", false);
    }
    if (operation == "request_drop") {
        return normalizeDecimalField(result, "amount", true)
            && normalizeDecimalField(result, "balance_before", true)
            && normalizeDecimalField(result, "balance_after", true);
    }
    return true;
}

/// Map a structured error code onto a terminal job status.
///
/// Only the `code` field decides this. A message is for a human; classifying on
/// its text is banned end to end (API_LOCK §9), and the specific trap it exists
/// to stop is an `outcome_unknown` whose sentence mentions cancelling being
/// filed as a safe `cancelled`.
std::string terminalStatusForCode(const std::string& code)
{
    if (code == "cancelled") {
        return "cancelled";
    }
    if (code == "outcome_unknown") {
        return "outcome_unknown";
    }
    return "failed";
}

} // namespace

// ---- job registry ----------------------------------------------------------

struct LezFaucetModule::Job {
    mutable std::mutex mutex;
    std::string id;
    std::string operation;
    std::string requestKey;
    std::string status = "queued";
    std::string phase;
    Json result;
    Json error;
    bool hasResult = false;
    bool hasError = false;
    bool mutating = false;
    bool cancelRequested = false;
    bool workerFinished = false;
    uint64_t operationToken = 0;
    std::thread worker;
};

LezFaucetModule::LezFaucetModule() = default;

LezFaucetModule::~LezFaucetModule()
{
    // Ordering here is the whole safety argument (API_LOCK §A1.7.1):
    //
    //   1. mark stopping, so no new job enters the FFI;
    //   2. flag every live job cancelled and ask Rust to abandon the active
    //      operation token;
    //   3. JOIN every worker, so no thread is inside an FFI call any more;
    //   4. only then destroy the client handle.
    //
    // Destroying the handle before the join is a use-after-free: a worker
    // blocked in `lez_faucet_request_drop` would keep using the client the
    // destructor had already freed. Do not reorder these steps.
    joinAllWorkers();

    if (LezFaucetClient* client = m_client.exchange(nullptr); client != nullptr) {
        lez_faucet_client_destroy(client);
    }
}

std::string LezFaucetModule::name() const
{
    return "lez_faucet";
}

std::string LezFaucetModule::version() const
{
    // Kept in step with faucet-module/metadata.json by hand; the two are
    // independent copies and drift silently if only one is bumped.
    return "0.3.1";
}

// ---- public API ------------------------------------------------------------

std::string LezFaucetModule::configure(const std::string& sequencerUrl)
{
    reapFinishedWorkers();
    if (m_stopping.load()) {
        return structuredError("module_stopping", "The faucet module is shutting down.");
    }

    std::string reason;
    if (!sequencerUrlIsWellFormed(sequencerUrl, reason)) {
        return structuredError("invalid_sequencer_url", reason);
    }

    // Reconfiguration while a job is live would swap the client out from under
    // a worker that is inside an FFI call with the old handle.
    {
        std::lock_guard<std::mutex> lock(m_jobsMutex);
        for (const auto& entry : m_jobs) {
            std::lock_guard<std::mutex> jobLock(entry.second->mutex);
            if (!isTerminal(entry.second->status)) {
                return structuredError("drop_in_progress",
                    "Wait for the running faucet operation to finish before reconfiguring.");
            }
        }
    }

    LezFaucetClientOutput output = lez_faucet_client_new(sequencerUrl.c_str());
    const std::string raw = takeFfiString(output.result_json);
    const std::optional<Json> parsed = parseJson(raw);
    if (!parsed.has_value() || !envelopeSucceeded(*parsed)) {
        if (output.client != nullptr) {
            // A handle returned alongside a failure is still ours to free.
            lez_faucet_client_destroy(output.client);
        }
        // Re-serialize rather than echoing the FFI text: what leaves this module
        // is always a document this module has parsed and understood.
        if (parsed.has_value() && parsed->at("error").at("code").isString()) {
            Json envelope = Json::object();
            envelope.set("ok", Json::boolean(false));
            envelope.set("error", parsed->at("error"));
            return envelope.dump();
        }
        return structuredError("invalid_sequencer_url",
            "The faucet could not be configured for that sequencer URL.");
    }
    if (output.client == nullptr) {
        return structuredError("internal_error",
            "The faucet reported success without returning a client.");
    }

    if (LezFaucetClient* previous = m_client.exchange(output.client); previous != nullptr) {
        // Safe: every job is terminal, so no worker is inside the FFI.
        lez_faucet_client_destroy(previous);
    }

    Json result = Json::object();
    result.set("configured", Json::boolean(true));
    result.set("sequencer_url", Json::string(sequencerUrl));
    Json envelope = Json::object();
    envelope.set("ok", Json::boolean(true));
    envelope.set("result", std::move(result));
    return envelope.dump();
}

std::string LezFaucetModule::getFaucetInfo()
{
    // No mutating-job permit is taken here, by design: a read must stay
    // answerable during a 300 s reconciliation (API_LOCK §A1.7.2).
    return startJob("faucet_info", {}, false, [this](const JobPtr&) {
        return runFfiCall([](LezFaucetClient* client) { return lez_faucet_get_info(client); });
    });
}

std::string LezFaucetModule::inspectRecipient(const std::string& address)
{
    std::string reason;
    if (!addressIsWellFormed(address, reason)) {
        return structuredError("invalid_public_account_id", reason);
    }
    return startJob("inspect_recipient", {}, false, [this, address](const JobPtr&) {
        return runFfiCall([&address](LezFaucetClient* client) {
            return lez_faucet_inspect_recipient(client, address.c_str());
        });
    });
}

std::string LezFaucetModule::requestDrop(const std::string& address, const std::string& requestKey)
{
    reapFinishedWorkers();
    if (m_stopping.load()) {
        return structuredError("module_stopping", "The faucet module is shutting down.");
    }
    if (!requestKeyIsWellFormed(requestKey)) {
        return structuredError("invalid_request_key", "The request key must be a lowercase UUID.");
    }
    std::string reason;
    if (!addressIsWellFormed(address, reason)) {
        return structuredError("invalid_public_account_id", reason);
    }
    if (m_client.load() == nullptr) {
        return structuredError("not_configured", "The faucet client is not configured.");
    }

    const std::string accountId = bareAccountId(address);

    // Claim the key first, under one lock, so two callers racing with the same
    // key cannot both get past this point and start two drops.
    std::string replayJobId;
    std::string replayStatus;
    {
        std::lock_guard<std::mutex> lock(m_tombstonesMutex);
        const auto found = m_tombstones.find(requestKey);
        if (found != m_tombstones.end()) {
            if (found->second.accountId != accountId) {
                return structuredError("idempotency_conflict",
                    "That request key was already used for a different address.");
            }
            if (found->second.jobId.empty()) {
                // Reserved microseconds ago by a concurrent duplicate that has
                // not published its job id yet.
                return structuredError("drop_in_progress", "That request is already running.");
            }
            replayJobId = found->second.jobId;
            replayStatus = found->second.status;
        } else {
            m_tombstones.emplace(requestKey, Tombstone{accountId, std::string(), "queued"});
        }
    }

    // A repeated key replays the ORIGINAL job envelope, job id included, and
    // starts nothing. Losing the job id here would strand the caller's only
    // handle on a claim that may already be on chain.
    if (!replayJobId.empty()) {
        if (const JobPtr job = findJob(replayJobId); job) {
            return startedJson(job);
        }
        // The job record was acknowledged and reaped; the tombstone outlives it.
        Json envelope = Json::object();
        envelope.set("ok", Json::boolean(true));
        envelope.set("job_id", Json::string(replayJobId));
        envelope.set("request_key", Json::string(requestKey));
        envelope.set("status", Json::string(replayStatus));
        return envelope.dump();
    }

    const auto abandonReservation = [this, &requestKey]() {
        // Removing a reservation that never started a job cannot resurrect a
        // claim: no operation ran, so no transaction can exist. Only a key that
        // reached a worker becomes a permanent tombstone.
        std::lock_guard<std::mutex> lock(m_tombstonesMutex);
        const auto found = m_tombstones.find(requestKey);
        if (found != m_tombstones.end() && found->second.jobId.empty()) {
            m_tombstones.erase(found);
        }
    };

    if (!reserveDropSlot()) {
        abandonReservation();
        return structuredError("drop_in_progress",
            "A faucet request is already running. Wait for it to finish.");
    }

    const std::string started = startJob("request_drop", requestKey, true,
        [this, address, requestKey](const JobPtr& job) {
            return runDrop(job, address, requestKey);
        });

    const std::optional<Json> parsed = parseJson(started);
    if (!parsed.has_value() || !envelopeSucceeded(*parsed)) {
        releaseDropSlot();
        abandonReservation();
        return started;
    }
    rememberRequestKey(requestKey, accountId, parsed->at("job_id").text());
    return started;
}

std::string LezFaucetModule::cancel(const std::string& jobId)
{
    reapFinishedWorkers();
    const JobPtr job = findJob(jobId);
    if (!job) {
        return structuredError("unknown_job", "No job with that id.");
    }

    uint64_t token = 0;
    {
        std::lock_guard<std::mutex> lock(job->mutex);
        if (!isTerminal(job->status)) {
            job->cancelRequested = true;
            token = job->operationToken;
            job->status = "cancelling";
        }
    }
    // Scoped by the operation token allocated for this job, so a cancel that
    // arrives after its drop finished cannot stop a later one.
    if (token != 0) {
        if (LezFaucetClient* client = m_client.load(); client != nullptr) {
            lez_faucet_cancel(client, token);
        }
    }
    return jobJson(job);
}

std::string LezFaucetModule::jobStatus(const std::string& jobId)
{
    reapFinishedWorkers();
    const JobPtr job = findJob(jobId);
    if (!job) {
        return structuredError("unknown_job", "No job with that id.");
    }
    refreshJobPhase(job);
    return jobJson(job);
}

std::string LezFaucetModule::jobResultAck(const std::string& jobId)
{
    reapFinishedWorkers();
    JobPtr job;
    std::thread worker;
    {
        std::lock_guard<std::mutex> lock(m_jobsMutex);
        const auto found = m_jobs.find(jobId);
        if (found == m_jobs.end()) {
            return structuredError("unknown_job", "No job with that id.");
        }
        job = found->second;
        std::lock_guard<std::mutex> jobLock(job->mutex);
        if (!isTerminal(job->status)) {
            return structuredError("job_not_terminal",
                "That job has not finished, so its result cannot be acknowledged.");
        }
        if (job->worker.joinable()) {
            worker = std::move(job->worker);
        }
        m_jobs.erase(found);
    }
    if (worker.joinable()) {
        worker.join();
    }
    // The request-key tombstone deliberately survives this. Releasing the
    // payload is not the same as forgetting that the key was used, and
    // forgetting it would let a bridge retry become a second claim
    // (API_LOCK §6, §A1.7.4).
    Json envelope = Json::object();
    envelope.set("ok", Json::boolean(true));
    envelope.set("job_id", Json::string(jobId));
    envelope.set("acknowledged", Json::boolean(true));
    envelope.set("reaped", Json::boolean(true));
    return envelope.dump();
}

std::string LezFaucetModule::shutdown()
{
    const bool alreadyStopping = m_stopping.load();
    // Same ordering as the destructor, and for the same reason: every worker
    // must be out of the FFI before the handle it is using is freed.
    joinAllWorkers();
    LezFaucetClient* client = m_client.exchange(nullptr);
    if (client != nullptr) {
        lez_faucet_client_destroy(client);
    }

    Json result = Json::object();
    result.set("stopped", Json::boolean(true));
    result.set("client_destroyed", Json::boolean(client != nullptr));
    result.set("already_stopping", Json::boolean(alreadyStopping));
    Json envelope = Json::object();
    envelope.set("ok", Json::boolean(true));
    envelope.set("result", std::move(result));
    return envelope.dump();
}

// ---- job machinery ---------------------------------------------------------

std::string LezFaucetModule::startJob(const std::string& operation, const std::string& requestKey,
                                      bool mutating, std::function<std::string(const JobPtr&)> work)
{
    reapFinishedWorkers();
    if (m_stopping.load()) {
        return structuredError("module_stopping", "The faucet module is shutting down.");
    }
    if (m_client.load() == nullptr) {
        return structuredError("not_configured", "The faucet client is not configured.");
    }

    auto job = std::make_shared<Job>();
    job->id = operation + "-" + std::to_string(m_nextJobId.fetch_add(1));
    job->operation = operation;
    job->requestKey = requestKey;
    job->mutating = mutating;
    // Tokens are allocated here, strictly increasing and never 0 (API_LOCK
    // §A1.7.5): 0 is the Rust "no operation" sentinel, and a reused token would
    // let a stale cancel abort somebody else's drop. Reads get no token because
    // no read FFI entry point is cancellable.
    if (mutating) {
        job->operationToken = m_nextOperationToken.fetch_add(1);
    }

    {
        std::lock_guard<std::mutex> lock(m_jobsMutex);
        if (m_jobs.size() >= kMaxRetainedJobs) {
            return structuredError("job_limit_reached",
                "Too many unacknowledged jobs. Acknowledge finished results before starting more.");
        }
        m_jobs.emplace(job->id, job);
    }

    // The thread handle must be in place before the worker can possibly finish.
    //
    // Holding job->mutex across both the spawn and the assignment is what
    // guarantees that: the worker's first act is to lock the same mutex, so it
    // blocks until the handle is stored. Spawning outside the lock instead
    // would let a worker that finished early be observed by a concurrent reap
    // or join as a default-constructed, non-joinable job->worker and skipped —
    // and the joinable handle assigned a moment later would then be destroyed
    // by ~Job with nothing having joined it, which calls std::terminate() and
    // takes the whole host application down with it.
    {
        std::lock_guard<std::mutex> lock(job->mutex);
        job->worker = std::thread([this, job, work = std::move(work)]() {
            bool ran = false;
            {
                std::lock_guard<std::mutex> workerLock(job->mutex);
                // A cancel that lands before the worker starts is honoured
                // without ever entering the FFI, which is what makes a
                // pre-submit cancellation provably free of any submitted
                // transaction.
                if (!job->cancelRequested && !m_stopping.load()) {
                    job->status = "running";
                    ran = true;
                }
            }
            settleJob(job, ran, ran ? work(job) : std::string());
        });
    }
    return startedJson(job);
}

void LezFaucetModule::settleJob(const JobPtr& job, bool ran, const std::string& raw)
{
    std::string finalStatus;
    {
        std::lock_guard<std::mutex> lock(job->mutex);
        if (!ran) {
            job->status = "cancelled";
            job->error = errorObject("cancelled",
                "The request was cancelled before anything was submitted.");
            job->hasError = true;
        } else if (const std::optional<Json> parsed = parseJson(raw); !parsed.has_value()) {
            job->status = "failed";
            job->error = errorObject("internal_error",
                "The faucet returned a result this app could not read.");
            job->hasError = true;
        } else if (envelopeSucceeded(*parsed)) {
            Json result = parsed->at("result");
            if (!normalizeResult(job->operation, result)) {
                job->status = "failed";
                job->error = errorObject("internal_error",
                    "The faucet returned an amount this app could not verify.");
                job->hasError = true;
            } else {
                // A cancel that arrives after the work succeeded is not a
                // cancellation: reporting one would hide a claim that is on
                // chain (API_LOCK §5).
                job->status = "succeeded";
                job->result = std::move(result);
                job->hasResult = true;
            }
        } else {
            const Json& error = parsed->at("error");
            if (!error.at("code").isString()) {
                job->status = "failed";
                job->error = errorObject("internal_error",
                    "The faucet reported a failure without a code.");
                job->hasError = true;
            } else {
                // The status comes from the structured code and from nothing
                // else. In particular an `outcome_unknown` whose message
                // mentions cancelling stays `outcome_unknown`.
                job->status = terminalStatusForCode(error.at("code").text());
                if (error.at("phase").isString()) {
                    job->phase = error.at("phase").text();
                }
                job->error = error;
                job->hasError = true;
            }
        }
        job->workerFinished = true;
        finalStatus = job->status;
    }

    if (job->mutating) {
        releaseDropSlot();
        recordTombstoneStatus(job->requestKey, finalStatus);
    }
}

void LezFaucetModule::reapFinishedWorkers()
{
    std::vector<JobPtr> jobs;
    {
        std::lock_guard<std::mutex> lock(m_jobsMutex);
        jobs.reserve(m_jobs.size());
        for (const auto& entry : m_jobs) {
            jobs.push_back(entry.second);
        }
    }

    std::vector<std::thread> finished;
    for (const JobPtr& job : jobs) {
        std::lock_guard<std::mutex> lock(job->mutex);
        if (job->workerFinished && job->worker.joinable()) {
            finished.emplace_back(std::move(job->worker));
        }
    }
    for (std::thread& worker : finished) {
        worker.join();
    }
}

void LezFaucetModule::joinAllWorkers()
{
    // Step 1: no new job may enter the FFI from here on.
    m_stopping.store(true);

    // Step 2: flag every live job cancelled, and collect the operation tokens
    // that Rust can actually act on.
    std::vector<JobPtr> jobs;
    std::vector<uint64_t> tokens;
    {
        std::lock_guard<std::mutex> lock(m_jobsMutex);
        jobs.reserve(m_jobs.size());
        for (const auto& entry : m_jobs) {
            jobs.push_back(entry.second);
        }
    }
    for (const JobPtr& job : jobs) {
        std::lock_guard<std::mutex> lock(job->mutex);
        if (!isTerminal(job->status)) {
            job->cancelRequested = true;
            if (job->operationToken != 0) {
                tokens.push_back(job->operationToken);
            }
        }
    }

    // Step 3: ask Rust to abandon them. A drop that has already submitted keeps
    // reconciling to its bound rather than pretending it was cancelled, so this
    // shortens shutdown where it honestly can and waits where it cannot.
    if (LezFaucetClient* client = m_client.load(); client != nullptr) {
        for (const uint64_t token : tokens) {
            lez_faucet_cancel(client, token);
        }
    }

    // Step 4: join every worker. After this loop no thread is inside an FFI
    // call, which is the precondition the caller needs before destroying the
    // client handle.
    std::vector<std::thread> workers;
    workers.reserve(jobs.size());
    for (const JobPtr& job : jobs) {
        std::lock_guard<std::mutex> lock(job->mutex);
        if (job->worker.joinable()) {
            workers.emplace_back(std::move(job->worker));
        }
    }
    for (std::thread& worker : workers) {
        worker.join();
    }
}

std::string LezFaucetModule::runFfiCall(const std::function<char*(LezFaucetClient*)>& call)
{
    LezFaucetClient* client = m_client.load();
    if (client == nullptr) {
        return errorEnvelope("not_configured", "The faucet client is not configured.");
    }
    const std::string result = takeFfiString(call(client));
    return result.empty()
        ? errorEnvelope("internal_error", "The faucet returned no result.")
        : result;
}

std::string LezFaucetModule::runDrop(const JobPtr& job, const std::string& address,
                                     const std::string& requestKey)
{
    uint64_t token = 0;
    {
        std::lock_guard<std::mutex> lock(job->mutex);
        if (job->cancelRequested || m_stopping.load()) {
            return errorEnvelope("cancelled",
                "The request was cancelled before anything was submitted.");
        }
        token = job->operationToken;
    }
    return runFfiCall([&address, &requestKey, token](LezFaucetClient* client) {
        return lez_faucet_request_drop(client, address.c_str(), requestKey.c_str(), token);
    });
}

void LezFaucetModule::refreshJobPhase(const JobPtr& job)
{
    // Snapshot the token under the job lock, then RELEASE it before the FFI
    // call. The Rust side only reads two atomics, but holding job->mutex
    // across any FFI call would let one slow poll stall every other reader of
    // this job — and the worker's own settle — behind it.
    uint64_t token = 0;
    {
        std::lock_guard<std::mutex> lock(job->mutex);
        // Only a live drop has a phase to poll. A terminal job keeps the last
        // phase it recorded, and a read job never has an operation token.
        if (!job->mutating || job->operationToken == 0 || isTerminal(job->status)) {
            return;
        }
        token = job->operationToken;
    }
    LezFaucetClient* client = m_client.load();
    if (client == nullptr) {
        return;
    }
    const std::string raw = takeFfiString(lez_faucet_current_phase(client, token));
    const std::optional<Json> parsed = parseJson(raw);
    if (!parsed.has_value() || !envelopeSucceeded(*parsed)) {
        // A phase is a courtesy, never a verdict: an unreadable poll changes
        // nothing about the job.
        return;
    }
    const Json& phase = parsed->at("result").at("phase");
    if (!phase.isString()) {
        // A null phase means "no live phase" — the drop has not started or has
        // just finished. Keep the last phase rather than blanking it, so the
        // envelope never moves backwards while the worker settles.
        return;
    }
    std::lock_guard<std::mutex> lock(job->mutex);
    // The worker may have settled while the poll was in flight. A terminal
    // job's phase belongs to its outcome; a stale poll must not overwrite it.
    if (!isTerminal(job->status)) {
        job->phase = phase.text();
    }
}

bool LezFaucetModule::reserveDropSlot()
{
    std::lock_guard<std::mutex> lock(m_dropMutex);
    if (m_dropActive) {
        return false;
    }
    m_dropActive = true;
    return true;
}

void LezFaucetModule::releaseDropSlot()
{
    std::lock_guard<std::mutex> lock(m_dropMutex);
    m_dropActive = false;
}

LezFaucetModule::JobPtr LezFaucetModule::findJob(const std::string& jobId) const
{
    std::lock_guard<std::mutex> lock(m_jobsMutex);
    const auto found = m_jobs.find(jobId);
    return found == m_jobs.end() ? JobPtr() : found->second;
}

void LezFaucetModule::rememberRequestKey(const std::string& requestKey, const std::string& address,
                                         const std::string& jobId)
{
    std::lock_guard<std::mutex> lock(m_tombstonesMutex);
    Tombstone& tombstone = m_tombstones[requestKey];
    tombstone.accountId = address;
    tombstone.jobId = jobId;
    if (tombstone.status.empty()) {
        tombstone.status = "queued";
    }
}

void LezFaucetModule::recordTombstoneStatus(const std::string& requestKey, const std::string& status)
{
    if (requestKey.empty()) {
        return;
    }
    std::lock_guard<std::mutex> lock(m_tombstonesMutex);
    const auto found = m_tombstones.find(requestKey);
    if (found != m_tombstones.end()) {
        found->second.status = status;
    }
}

std::string LezFaucetModule::takeFfiString(char* value)
{
    if (value == nullptr) {
        return {};
    }
    // Every string this library hands out is freed exactly once, here, whatever
    // the caller does with the copy.
    std::string result(value);
    lez_faucet_string_free(value);
    return result;
}

std::string LezFaucetModule::structuredError(const std::string& code, const std::string& message)
{
    return errorEnvelope(code, message);
}

std::string LezFaucetModule::jobJson(const JobPtr& job)
{
    std::lock_guard<std::mutex> lock(job->mutex);
    Json envelope = Json::object();
    envelope.set("ok", Json::boolean(true));
    envelope.set("job_id", Json::string(job->id));
    envelope.set("operation", Json::string(job->operation));
    if (!job->requestKey.empty()) {
        envelope.set("request_key", Json::string(job->requestKey));
    }
    envelope.set("status", Json::string(job->status));
    if (!job->phase.empty()) {
        envelope.set("phase", Json::string(job->phase));
    }
    envelope.set("cancel_requested", Json::boolean(job->cancelRequested));
    if (job->hasResult) {
        envelope.set("result", job->result);
    }
    if (job->hasError) {
        envelope.set("error", job->error);
    }
    return envelope.dump();
}

std::string LezFaucetModule::startedJson(const JobPtr& job)
{
    std::lock_guard<std::mutex> lock(job->mutex);
    Json envelope = Json::object();
    envelope.set("ok", Json::boolean(true));
    envelope.set("job_id", Json::string(job->id));
    if (!job->requestKey.empty()) {
        envelope.set("request_key", Json::string(job->requestKey));
    }
    envelope.set("status", Json::string(job->status));
    return envelope.dump();
}

bool LezFaucetModule::isTerminal(const std::string& status)
{
    return status == "succeeded" || status == "failed" || status == "cancelled"
        || status == "outcome_unknown";
}
