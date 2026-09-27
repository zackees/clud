#ifndef CLUD_INSTALLER_TTY_HPP
#define CLUD_INSTALLER_TTY_HPP

#include <algorithm>
#include <string>

namespace clud_installer {

struct MenuWindow {
  size_t begin;
  size_t end;
};

inline MenuWindow menu_window(size_t selected, size_t count, size_t visible) {
  if (visible == 0 || count <= visible) return {0, count};
  const size_t begin = selected >= visible ? selected - visible + 1 : 0;
  return {begin, std::min(count, begin + visible)};
}

inline std::string safe_crlf(const std::string &input) {
  std::string output;
  output.reserve(input.size() + 8);
  for (size_t i = 0; i < input.size(); ++i) {
    const unsigned char c = static_cast<unsigned char>(input[i]);
    if (c == '\r') {
      if (i + 1 < input.size() && input[i + 1] == '\n') {
        output.append("\r\n");
        ++i;
      }
    } else if (c == '\n') {
      output.append("\r\n");
    } else if (c >= 0x20 && c != 0x7f) {
      output.push_back(static_cast<char>(c));
    }
  }
  return output;
}

inline std::string menu_frame(const std::string &title,
                              const std::string *versions, size_t count,
                              size_t selected, size_t visible) {
  const MenuWindow window = menu_window(selected, count, visible);
  std::string frame;
  const auto line = [&frame](const std::string &text) {
    frame += safe_crlf(text);
    frame += "\r\n";
  };
  line(title);
  line("Use up/down arrows (or j/k); Enter selects; Esc cancels.");
  frame += "\r\n";
  if (window.begin > 0)
    line("  ... " + std::to_string(window.begin) + " more above");
  for (size_t i = window.begin; i < window.end; ++i) {
    const std::string row_line = std::string(i == selected ? "> [*] " : "  [ ] ") + versions[i];
    line(row_line);
  }
  if (window.end < count)
    line("  ... " + std::to_string(count - window.end) + " more below");
  return frame;
}

inline bool valid_crlf_stream(const std::string &text) {
  for (size_t i = 0; i < text.size(); ++i) {
    if (text[i] == '\n' && (i == 0 || text[i - 1] != '\r')) return false;
    if (text[i] == '\r' && (i + 1 == text.size() || text[i + 1] != '\n')) return false;
  }
  return true;
}

}  // namespace clud_installer

#endif  // CLUD_INSTALLER_TTY_HPP
