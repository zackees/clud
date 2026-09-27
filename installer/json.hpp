#ifndef CLUD_INSTALLER_JSON_HPP
#define CLUD_INSTALLER_JSON_HPP

#include <cstdint>
#include <string>
#include <utility>
#include <vector>

namespace clud_installer {

struct Json {
  enum class Type { Null, Boolean, Number, String, Array, Object } type = Type::Null;
  std::string scalar;
  std::vector<Json> array;
  std::vector<std::pair<std::string, Json>> object;

  const Json *get(const std::string &key) const {
    if (type != Type::Object) return nullptr;
    for (const auto &entry : object)
      if (entry.first == key) return &entry.second;
    return nullptr;
  }

  const std::string *string() const {
    return type == Type::String ? &scalar : nullptr;
  }
};

class JsonParser {
 public:
  explicit JsonParser(const std::string &input) : input_(input) {}

  bool parse(Json *out, std::string *error) {
    error_ = error;
    skip_space();
    if (!value(out, 0)) return false;
    skip_space();
    if (position_ != input_.size()) return fail("trailing bytes after JSON value");
    return true;
  }

 private:
  const std::string &input_;
  size_t position_ = 0;
  std::string *error_ = nullptr;

  bool fail(const char *message) {
    if (error_) *error_ = std::string(message) + " at byte " + std::to_string(position_);
    return false;
  }

  void skip_space() {
    while (position_ < input_.size() &&
           (input_[position_] == ' ' || input_[position_] == '\t' ||
            input_[position_] == '\r' || input_[position_] == '\n'))
      ++position_;
  }

  bool take(char expected) {
    if (position_ < input_.size() && input_[position_] == expected) {
      ++position_;
      return true;
    }
    return false;
  }

  static void append_utf8(std::string *out, uint32_t cp) {
    if (cp <= 0x7f) {
      out->push_back(static_cast<char>(cp));
    } else if (cp <= 0x7ff) {
      out->push_back(static_cast<char>(0xc0 | (cp >> 6)));
      out->push_back(static_cast<char>(0x80 | (cp & 0x3f)));
    } else if (cp <= 0xffff) {
      out->push_back(static_cast<char>(0xe0 | (cp >> 12)));
      out->push_back(static_cast<char>(0x80 | ((cp >> 6) & 0x3f)));
      out->push_back(static_cast<char>(0x80 | (cp & 0x3f)));
    } else {
      out->push_back(static_cast<char>(0xf0 | (cp >> 18)));
      out->push_back(static_cast<char>(0x80 | ((cp >> 12) & 0x3f)));
      out->push_back(static_cast<char>(0x80 | ((cp >> 6) & 0x3f)));
      out->push_back(static_cast<char>(0x80 | (cp & 0x3f)));
    }
  }

  bool hex4(uint32_t *value) {
    if (input_.size() - position_ < 4) return fail("short unicode escape");
    uint32_t result = 0;
    for (int i = 0; i < 4; ++i) {
      const char c = input_[position_++];
      result <<= 4;
      if (c >= '0' && c <= '9') result |= static_cast<uint32_t>(c - '0');
      else if (c >= 'a' && c <= 'f') result |= static_cast<uint32_t>(c - 'a' + 10);
      else if (c >= 'A' && c <= 'F') result |= static_cast<uint32_t>(c - 'A' + 10);
      else return fail("invalid unicode escape");
    }
    *value = result;
    return true;
  }

  bool string(std::string *out) {
    if (!take('"')) return fail("expected string");
    while (position_ < input_.size()) {
      const unsigned char c = static_cast<unsigned char>(input_[position_++]);
      if (c == '"') return true;
      if (c < 0x20) return fail("unescaped control character");
      if (c != '\\') {
        out->push_back(static_cast<char>(c));
        continue;
      }
      if (position_ >= input_.size()) return fail("unfinished escape");
      switch (input_[position_++]) {
        case '"': out->push_back('"'); break;
        case '\\': out->push_back('\\'); break;
        case '/': out->push_back('/'); break;
        case 'b': out->push_back('\b'); break;
        case 'f': out->push_back('\f'); break;
        case 'n': out->push_back('\n'); break;
        case 'r': out->push_back('\r'); break;
        case 't': out->push_back('\t'); break;
        case 'u': {
          uint32_t cp;
          if (!hex4(&cp)) return false;
          if (cp >= 0xd800 && cp <= 0xdbff) {
            if (input_.size() - position_ < 6 || input_[position_] != '\\' ||
                input_[position_ + 1] != 'u') return fail("invalid surrogate pair");
            position_ += 2;
            uint32_t low;
            if (!hex4(&low) || low < 0xdc00 || low > 0xdfff)
              return fail("invalid surrogate pair");
            cp = 0x10000 + ((cp - 0xd800) << 10) + (low - 0xdc00);
          } else if (cp >= 0xdc00 && cp <= 0xdfff) {
            return fail("orphan low surrogate");
          }
          append_utf8(out, cp);
          break;
        }
        default: return fail("invalid escape");
      }
    }
    return fail("unterminated string");
  }

  bool value(Json *out, unsigned depth) {
    if (depth > 64) return fail("JSON nesting limit exceeded");
    skip_space();
    if (position_ >= input_.size()) return fail("expected value");
    const char c = input_[position_];
    if (c == '"') {
      out->type = Json::Type::String;
      return string(&out->scalar);
    }
    if (c == '{') return object(out, depth + 1);
    if (c == '[') return array(out, depth + 1);
    if (c == 't' && literal("true")) {
      out->type = Json::Type::Boolean;
      out->scalar = "true";
      return true;
    }
    if (c == 'f' && literal("false")) {
      out->type = Json::Type::Boolean;
      out->scalar = "false";
      return true;
    }
    if (c == 'n' && literal("null")) {
      out->type = Json::Type::Null;
      return true;
    }
    return number(out);
  }

  bool literal(const char *text) {
    size_t i = 0;
    while (text[i]) {
      if (position_ + i >= input_.size() || input_[position_ + i] != text[i]) return false;
      ++i;
    }
    position_ += i;
    return true;
  }

  bool number(Json *out) {
    const size_t begin = position_;
    take('-');
    if (!take('0')) {
      if (position_ >= input_.size() || input_[position_] < '1' || input_[position_] > '9')
        return fail("invalid number");
      while (position_ < input_.size() && input_[position_] >= '0' && input_[position_] <= '9')
        ++position_;
    }
    if (take('.')) {
      const size_t digits = position_;
      while (position_ < input_.size() && input_[position_] >= '0' && input_[position_] <= '9')
        ++position_;
      if (digits == position_) return fail("invalid fraction");
    }
    if (take('e') || take('E')) {
      if (!take('+')) take('-');
      const size_t digits = position_;
      while (position_ < input_.size() && input_[position_] >= '0' && input_[position_] <= '9')
        ++position_;
      if (digits == position_) return fail("invalid exponent");
    }
    out->type = Json::Type::Number;
    out->scalar = input_.substr(begin, position_ - begin);
    return true;
  }

  bool array(Json *out, unsigned depth) {
    take('[');
    out->type = Json::Type::Array;
    skip_space();
    if (take(']')) return true;
    while (true) {
      Json child;
      if (!value(&child, depth)) return false;
      out->array.push_back(std::move(child));
      skip_space();
      if (take(']')) return true;
      if (!take(',')) return fail("expected comma in array");
    }
  }

  bool object(Json *out, unsigned depth) {
    take('{');
    out->type = Json::Type::Object;
    skip_space();
    if (take('}')) return true;
    while (true) {
      skip_space();
      std::string key;
      if (!string(&key)) return false;
      for (const auto &entry : out->object)
        if (entry.first == key) return fail("duplicate object key");
      skip_space();
      if (!take(':')) return fail("expected colon in object");
      Json child;
      if (!value(&child, depth)) return false;
      out->object.emplace_back(std::move(key), std::move(child));
      skip_space();
      if (take('}')) return true;
      if (!take(',')) return fail("expected comma in object");
    }
  }
};

}  // namespace clud_installer

#endif  // CLUD_INSTALLER_JSON_HPP
