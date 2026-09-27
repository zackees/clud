#include <cosmo.h>

#include <cstdio>
#include <cstring>
#include <cctype>
#include <cstdint>
#include <string>
#include <vector>

#include <sys/utsname.h>
#include <sys/stat.h>
#include <pwd.h>
#include <poll.h>
#include <termios.h>
#include <unistd.h>

#include "libc/nt/console.h"
#include "libc/nt/enum/consolemodeflags.h"
#include "libc/nt/runtime.h"
#include "libc/zip.h"
#include "third_party/zlib/zlib.h"

#include "json.hpp"
#include "sha256.hpp"
#include "tty.hpp"

namespace {

constexpr char kManifestUrl[] =
    "https://zackees.github.io/clud/install/manifest.json";

struct Platform {
  std::string os;
  std::string arch;
};

struct Release {
  std::string version;
  std::string filename;
  std::string url;
  std::string sha256;
  std::string media_type;
  uint64_t size = 0;
};

bool parse_uint64(const clud_installer::Json *value, uint64_t *out) {
  if (!value || value->type != clud_installer::Json::Type::Number ||
      value->scalar.empty())
    return false;
  uint64_t number = 0;
  for (char c : value->scalar) {
    if (c < '0' || c > '9' || number > (UINT64_MAX - (c - '0')) / 10) return false;
    number = number * 10 + static_cast<unsigned>(c - '0');
  }
  *out = number;
  return true;
}

bool valid_sha256(const std::string &value) {
  if (value.size() != 64) return false;
  for (char c : value)
    if (!((c >= '0' && c <= '9') || (c >= 'a' && c <= 'f'))) return false;
  return true;
}

bool safe_asset_url(const std::string &url) {
  constexpr char prefix[] = "https://github.com/zackees/clud/releases/download/";
  if (url.compare(0, sizeof(prefix) - 1, prefix) != 0) return false;
  for (char c : url)
    if (std::iscntrl(static_cast<unsigned char>(c)) || c == '\'' || c == '"' || c == '`')
      return false;
  return true;
}

bool host_platform(Platform *platform) {
  if (IsWindows()) {
    platform->os = "windows";
    uint16_t process_machine = 0;
    uint16_t native_machine = 0;
    if (!IsWow64Process2(GetCurrentProcess(), &process_machine, &native_machine)) {
      std::fputs("clud-installer: cannot determine the native Windows architecture\n", stderr);
      return false;
    }
    const uint16_t machine = native_machine ? native_machine : process_machine;
    if (machine == 0x8664) platform->arch = "x86_64";
    else if (machine == 0xAA64) platform->arch = "aarch64";
    else {
      std::fprintf(stderr, "clud-installer: unsupported Windows machine type 0x%04x\n", machine);
      return false;
    }
    return true;
  }

  struct utsname info;
  if (uname(&info) != 0) {
    std::fputs("clud-installer: cannot determine the host platform with uname\n", stderr);
    return false;
  }
  if (IsXnu()) platform->os = "darwin";
  else if (IsLinux()) platform->os = "linux";
  else {
    std::fputs("clud-installer: this operating system is not supported\n", stderr);
    return false;
  }
  const std::string machine(info.machine);
  if (machine == "x86_64" || machine == "amd64") platform->arch = "x86_64";
  else if (machine == "aarch64" || machine == "arm64") platform->arch = "aarch64";
  else {
    std::fprintf(stderr, "clud-installer: unsupported machine architecture %s\n", info.machine);
    return false;
  }
  return true;
}

bool get_string(const clud_installer::Json *object, const char *key,
                std::string *value) {
  const auto *field = object ? object->get(key) : nullptr;
  if (!field || !field->string()) return false;
  *value = *field->string();
  return true;
}

bool load_releases(const clud_installer::Json &catalog, const Platform &platform,
                   std::vector<Release> *result, std::string *latest_stable,
                   std::string *error);

bool read_text_file(const std::string &path, std::string *text) {
  FILE *input = std::fopen(path.c_str(), "rb");
  if (!input) return false;
  if (std::fseek(input, 0, SEEK_END) != 0) {
    std::fclose(input);
    return false;
  }
  const long length = std::ftell(input);
  if (length < 0 || length > 5 * 1024 * 1024 || std::fseek(input, 0, SEEK_SET) != 0) {
    std::fclose(input);
    return false;
  }
  text->resize(static_cast<size_t>(length));
  const bool read_ok = text->empty() ||
      std::fread(&(*text)[0], 1, text->size(), input) == text->size();
  const bool close_ok = std::fclose(input) == 0;
  return read_ok && close_ok;
}

bool hash_file(const std::string &path, uint64_t *size, std::string *sha256) {
  FILE *input = std::fopen(path.c_str(), "rb");
  if (!input) return false;
  clud_installer::Sha256 hash;
  uint64_t total = 0;
  unsigned char buffer[64 * 1024];
  size_t count;
  while ((count = std::fread(buffer, 1, sizeof(buffer), input)) != 0) {
    if (total > UINT64_MAX - count) {
      std::fclose(input);
      return false;
    }
    total += count;
    hash.update(buffer, count);
  }
  const bool failed = std::ferror(input) != 0;
  const bool closed = std::fclose(input) == 0;
  if (failed || !closed) return false;
  *size = total;
  *sha256 = hash.finish();
  return true;
}

std::string shell_quote(const std::string &value) {
  if (IsWindows()) {
    std::string quoted = "\"";
    for (char c : value) {
      if (c == '"') quoted.push_back('^');
      quoted.push_back(c);
    }
    quoted.push_back('"');
    return quoted;
  }
  std::string quoted = "'";
  for (char c : value) {
    if (c == '\'') quoted += "'\\''";
    else quoted.push_back(c);
  }
  quoted.push_back('\'');
  return quoted;
}

std::string path_join(const std::string &left, const std::string &right) {
  if (left.empty()) return right;
  const char last = left.back();
  return left + (last == '/' || last == '\\' ? "" : (IsWindows() ? "\\" : "/")) + right;
}

bool make_temp_directory(std::string *directory) {
  const char *temp = std::getenv(IsWindows() ? "TEMP" : "TMPDIR");
  const std::string base = temp && *temp ? temp : (IsWindows() ? "." : "/tmp");
  for (unsigned attempt = 0; attempt < 20; ++attempt) {
    const std::string name = "clud-installer-" + std::to_string(getpid()) + "-" +
                             std::to_string(attempt);
    *directory = path_join(base, name);
    if (mkdir(directory->c_str(), 0700) == 0) return true;
    struct stat info;
    if (stat(directory->c_str(), &info) == 0) continue;
    return false;
  }
  return false;
}

bool download_asset(const Release &release, const std::string &destination) {
  if (!safe_asset_url(release.url)) return false;
  const std::string curl = IsWindows() ? "curl.exe" : "curl";
  const std::string scheme = IsWindows() ? "\"=https\"" : "'=https'";
  const std::string command = curl + " -fLsS --retry 2 --proto " + scheme +
                              " --tlsv1.2 --output " + shell_quote(destination) +
                              " " + shell_quote(release.url);
  return std::system(command.c_str()) == 0;
}

bool read_binary_file(const std::string &path, std::vector<unsigned char> *bytes) {
  struct stat info;
  if (stat(path.c_str(), &info) != 0 || info.st_size < 0 ||
      static_cast<uint64_t>(info.st_size) > 256ULL * 1024 * 1024)
    return false;
  FILE *input = std::fopen(path.c_str(), "rb");
  if (!input) return false;
  bytes->resize(static_cast<size_t>(info.st_size));
  const bool read_ok = bytes->empty() ||
      std::fread(bytes->data(), 1, bytes->size(), input) == bytes->size();
  const bool closed = std::fclose(input) == 0;
  return read_ok && closed;
}

bool executable_bytes(const std::vector<unsigned char> &bytes, const Platform &platform) {
  if (platform.os == "windows") return bytes.size() >= 2 && bytes[0] == 'M' && bytes[1] == 'Z';
  if (platform.os == "linux")
    return bytes.size() >= 4 && bytes[0] == 0x7f && bytes[1] == 'E' &&
           bytes[2] == 'L' && bytes[3] == 'F';
  return bytes.size() >= 4 && bytes[0] == 0xcf && bytes[1] == 0xfa &&
         bytes[2] == 0xed && bytes[3] == 0xfe;
}

bool extract_wheel_executable(const std::vector<unsigned char> &wheel,
                              const Release &release, const Platform &platform,
                              std::vector<unsigned char> *executable,
                              std::string *error) {
  if (wheel.size() < kZipCdirHdrMinSize) {
    *error = "wheel file is too short to be a ZIP archive";
    return false;
  }
  const size_t tail = wheel.size() > 65557 ? wheel.size() - 65557 : 0;
  size_t eocd = wheel.size();
  for (size_t at = wheel.size() - kZipCdirHdrMinSize + 1; at-- > tail;) {
    if (ZIP_CDIR_MAGIC(wheel.data() + at) == static_cast<uint32_t>(kZipCdirHdrMagic)) {
      const uint16_t comment = ZIP_CDIR_COMMENTSIZE(wheel.data() + at);
      if (at + kZipCdirHdrMinSize + comment == wheel.size()) {
        eocd = at;
        break;
      }
    }
  }
  if (eocd == wheel.size()) {
    *error = "wheel has no valid ZIP central directory";
    return false;
  }
  const unsigned char *end = wheel.data() + eocd;
  const uint16_t entries = ZIP_CDIR_RECORDS(end);
  const uint32_t directory_size = ZIP_CDIR_SIZE(end);
  const uint32_t directory_offset = ZIP_CDIR_OFFSET(end);
  if (directory_offset > eocd || directory_size > eocd - directory_offset ||
      directory_offset + directory_size > eocd) {
    *error = "wheel central directory is outside the archive";
    return false;
  }
  const std::string member = "clud-" + release.version + ".data/scripts/" +
                             (platform.os == "windows" ? "clud.exe" : "clud");
  const unsigned char *cursor = wheel.data() + directory_offset;
  const unsigned char *limit = cursor + directory_size;
  const unsigned char *selected = nullptr;
  for (uint16_t i = 0; i < entries; ++i) {
    if (static_cast<size_t>(limit - cursor) < kZipCfileHdrMinSize ||
        ZIP_CFILE_MAGIC(cursor) != static_cast<uint32_t>(kZipCfileHdrMagic)) {
      *error = "wheel central directory contains an invalid entry";
      return false;
    }
    const uint16_t name_size = ZIP_CFILE_NAMESIZE(cursor);
    const uint16_t extra_size = ZIP_CFILE_EXTRASIZE(cursor);
    const uint16_t comment_size = ZIP_CFILE_COMMENTSIZE(cursor);
    const size_t header_size = kZipCfileHdrMinSize + name_size + extra_size + comment_size;
    if (header_size > static_cast<size_t>(limit - cursor)) {
      *error = "wheel central directory entry is truncated";
      return false;
    }
    const std::string name(ZIP_CFILE_NAME(cursor), name_size);
    if (name == member) {
      if (selected) {
        *error = "wheel contains more than one clud executable";
        return false;
      }
      selected = cursor;
    }
    cursor += header_size;
  }
  if (!selected) {
    *error = "wheel does not contain the selected clud executable";
    return false;
  }
  const uint16_t flags = ZIP_CFILE_GENERALFLAG(selected);
  const uint16_t method = ZIP_CFILE_COMPRESSIONMETHOD(selected);
  const uint32_t crc = ZIP_CFILE_CRC32(selected);
  const uint32_t compressed_size = ZIP_CFILE_COMPRESSEDSIZE(selected);
  const uint32_t uncompressed_size = ZIP_CFILE_UNCOMPRESSEDSIZE(selected);
  const uint32_t local_offset = ZIP_CFILE_OFFSET(selected);
  if ((flags & 1) || compressed_size == UINT32_MAX || uncompressed_size == UINT32_MAX ||
      local_offset > wheel.size() || wheel.size() - local_offset < kZipLfileHdrMinSize) {
    *error = "wheel executable uses an unsupported ZIP feature";
    return false;
  }
  const unsigned char *local = wheel.data() + local_offset;
  if (ZIP_READ32(local) != static_cast<uint32_t>(kZipLfileHdrMagic)) {
    *error = "wheel executable has an invalid local ZIP header";
    return false;
  }
  const uint16_t local_name = ZIP_READ16(local + kZipLfileOffsetNamesize);
  const uint16_t local_extra = ZIP_READ16(local + 28);
  const size_t data_offset = static_cast<size_t>(local_offset) + kZipLfileHdrMinSize +
                             local_name + local_extra;
  if (data_offset > wheel.size() || compressed_size > wheel.size() - data_offset ||
      uncompressed_size > 128U * 1024U * 1024U) {
    *error = "wheel executable data is truncated or unreasonably large";
    return false;
  }
  executable->resize(uncompressed_size);
  const unsigned char *compressed = wheel.data() + data_offset;
  if (method == kZipCompressionNone) {
    if (compressed_size != uncompressed_size) {
      *error = "stored wheel executable has inconsistent lengths";
      return false;
    }
    for (size_t i = 0; i < uncompressed_size; ++i) (*executable)[i] = compressed[i];
  } else if (method == kZipCompressionDeflate) {
    z_stream stream{};
    stream.next_in = const_cast<Bytef *>(compressed);
    stream.avail_in = compressed_size;
    stream.next_out = executable->data();
    stream.avail_out = uncompressed_size;
    if (inflateInit2(&stream, -MAX_WBITS) != Z_OK) {
      *error = "could not initialize ZIP decompression";
      return false;
    }
    const int status = inflate(&stream, Z_FINISH);
    const bool ended = inflateEnd(&stream) == Z_OK;
    if (status != Z_STREAM_END || !ended || stream.total_out != uncompressed_size ||
        stream.total_in != compressed_size) {
      *error = "wheel executable failed ZIP decompression";
      return false;
    }
  } else {
    *error = "wheel executable uses an unsupported compression method";
    return false;
  }
  if (crc32(0, executable->data(), static_cast<uInt>(executable->size())) != crc ||
      !executable_bytes(*executable, platform)) {
    *error = "extracted wheel member failed its CRC or executable-format check";
    return false;
  }
  return true;
}

struct InstallPlan {
  std::string directory;
  std::string executable;
  std::string shell;
  std::vector<std::string> profiles;
  std::string existing_clud;
  bool path_update = false;
  bool verify_interactive_shell = false;
};

std::string environment(const char *name) {
  const char *value = std::getenv(name);
  return value ? value : "";
}

bool path_exists(const std::string &path, bool *is_directory = nullptr) {
  struct stat info;
  if (stat(path.c_str(), &info) != 0) return false;
  if (is_directory) *is_directory = S_ISDIR(info.st_mode);
  return true;
}

std::string shell_name(const std::string &shell) {
  const size_t slash = shell.find_last_of('/');
  return shell.substr(slash == std::string::npos ? 0 : slash + 1);
}

std::string default_shell(const Platform &platform,
                          const std::string &account_shell = "") {
  if (!account_shell.empty()) return account_shell;
  return platform.os == "darwin" ? "/bin/zsh" : "/bin/bash";
}

std::vector<std::string> shell_profiles(const std::string &home,
                                        const std::string &shell,
                                        bool bash_profile_exists,
                                        bool bash_login_exists) {
  const std::string name = shell_name(shell);
  if (name == "bash") {
    const std::string login = bash_profile_exists ? ".bash_profile"
        : bash_login_exists ? ".bash_login" : ".profile";
    return {path_join(home, login), path_join(home, ".bashrc")};
  }
  if (name == "zsh")
    return {path_join(home, ".zprofile"), path_join(home, ".zshrc")};
  if (name == "fish")
    return {path_join(home, ".config/fish/conf.d/clud-path.fish")};
  return {path_join(home, ".profile")};
}

bool path_equal(std::string left, std::string right) {
  if (IsWindows()) {
    for (char &c : left) {
      if (c == '/') c = '\\';
      c = static_cast<char>(std::tolower(static_cast<unsigned char>(c)));
    }
    for (char &c : right) {
      if (c == '/') c = '\\';
      c = static_cast<char>(std::tolower(static_cast<unsigned char>(c)));
    }
    while (left.size() > 3 && left.back() == '\\') left.pop_back();
    while (right.size() > 3 && right.back() == '\\') right.pop_back();
  }
  return left == right;
}

std::vector<std::string> split_path(const std::string &path) {
  std::vector<std::string> parts;
  const char separator = IsWindows() ? ';' : ':';
  size_t begin = 0;
  while (begin <= path.size()) {
    const size_t end = path.find(separator, begin);
    const std::string part = path.substr(begin, end == std::string::npos ? end : end - begin);
    if (!part.empty()) parts.push_back(part);
    if (end == std::string::npos) break;
    begin = end + 1;
  }
  return parts;
}

std::string find_existing_clud(const std::string &path) {
  const std::string name = IsWindows() ? "clud.exe" : "clud";
  for (const auto &part : split_path(path)) {
    const std::string candidate = path_join(part, name);
    if (path_exists(candidate)) return candidate;
  }
  return "";
}

bool first_path_is(const std::string &path, const std::string &directory) {
  const auto parts = split_path(path);
  return !parts.empty() && path_equal(parts.front(), directory);
}

std::string shell_double_quote(const std::string &value) {
  std::string result;
  for (char c : value) {
    if (c == '\\' || c == '"' || c == '$' || c == '`') result.push_back('\\');
    result.push_back(c);
  }
  return result;
}

std::string path_profile_snippet(const std::string &shell,
                                 const std::string &directory) {
  std::string snippet = "# clud-installer managed PATH\n";
  const std::string quoted = shell_double_quote(directory);
  if (shell_name(shell) == "fish") {
    snippet += "fish_add_path --prepend \"" + quoted + "\"\n";
  } else {
    snippet += "case \"$PATH\" in\n  \"" + quoted + ":\"*) ;;\n";
    snippet += "  *) PATH=\"" + quoted + ":$PATH\"; export PATH ;;\nesac\n";
  }
  return snippet;
}

bool make_install_plan(const Platform &platform, InstallPlan *plan,
                       std::string *error) {
  const std::string home = IsWindows() ? environment("USERPROFILE") : environment("HOME");
  if (home.empty()) {
    *error = "the home directory environment variable is missing";
    return false;
  }
  if (IsWindows()) {
    std::string local = environment("LOCALAPPDATA");
    if (local.empty()) local = path_join(home, "AppData\\Local");
    plan->directory = path_join(local, "Programs\\clud\\bin");
    plan->executable = path_join(plan->directory, "clud.exe");
  } else {
    plan->directory = path_join(home, ".local/bin");
    plan->executable = path_join(plan->directory, "clud");
    plan->shell = environment("SHELL");
    if (plan->shell.empty() || access(plan->shell.c_str(), X_OK) != 0) {
      const struct passwd *account = getpwuid(getuid());
      const std::string account_shell = account && account->pw_shell
          ? account->pw_shell : "";
      plan->shell = default_shell(platform, account_shell);
      if (access(plan->shell.c_str(), X_OK) != 0) {
        plan->shell = platform.os == "darwin" ? "/bin/bash" : "/bin/sh";
        if (access(plan->shell.c_str(), X_OK) != 0) {
          *error = "could not determine an executable login shell";
          return false;
        }
      }
    }
    const std::string bash_profile = path_join(home, ".bash_profile");
    const std::string bash_login = path_join(home, ".bash_login");
    plan->profiles = shell_profiles(home, plan->shell, path_exists(bash_profile),
                                    path_exists(bash_login));
    const std::string name = shell_name(plan->shell);
    plan->verify_interactive_shell = name == "bash" || name == "zsh" || name == "fish";
  }
  const std::string path = environment("PATH");
  plan->existing_clud = find_existing_clud(path);
  plan->path_update = !first_path_is(path, plan->directory);
  if (!IsWindows()) {
    for (const auto &profile : plan->profiles) {
      std::string contents;
      if (!read_text_file(profile, &contents) ||
          contents.find(path_profile_snippet(plan->shell, plan->directory)) ==
              std::string::npos) {
        plan->path_update = true;
        break;
      }
    }
  }
  (void)platform;
  return true;
}

bool ensure_directory(const std::string &path) {
  if (path.empty()) return false;
  std::string current;
  size_t at = 0;
  if (path[0] == '/' || path[0] == '\\') {
    current = path.substr(0, 1);
    at = 1;
  } else if (IsWindows() && path.size() > 2 && path[1] == ':') {
    current = path.substr(0, 2);
    at = 2;
    if (at < path.size() && (path[at] == '/' || path[at] == '\\')) {
      current.push_back(path[at++]);
    }
  }
  while (at <= path.size()) {
    const size_t end = path.find_first_of(IsWindows() ? "/\\" : "/", at);
    const std::string component = path.substr(at, end == std::string::npos ? end : end - at);
    if (!component.empty()) {
      current = current.empty() ? component : path_join(current, component);
      if (mkdir(current.c_str(), 0700) != 0) {
        bool is_dir = false;
        if (!path_exists(current, &is_dir) || !is_dir) return false;
      }
    }
    if (end == std::string::npos) break;
    at = end + 1;
  }
  bool is_dir = false;
  return path_exists(path, &is_dir) && is_dir;
}

std::string base64_utf16le_ascii(const std::string &input) {
  std::vector<unsigned char> bytes;
  bytes.reserve(input.size() * 2);
  for (unsigned char c : input) {
    bytes.push_back(c);
    bytes.push_back(0);
  }
  static constexpr char alphabet[] =
      "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  std::string output;
  for (size_t i = 0; i < bytes.size(); i += 3) {
    const size_t remain = bytes.size() - i;
    const uint32_t chunk = (static_cast<uint32_t>(bytes[i]) << 16) |
                           (remain > 1 ? static_cast<uint32_t>(bytes[i + 1]) << 8 : 0) |
                           (remain > 2 ? bytes[i + 2] : 0);
    output.push_back(alphabet[(chunk >> 18) & 63]);
    output.push_back(alphabet[(chunk >> 12) & 63]);
    output.push_back(remain > 1 ? alphabet[(chunk >> 6) & 63] : '=');
    output.push_back(remain > 2 ? alphabet[chunk & 63] : '=');
  }
  return output;
}

bool run_capture(const std::string &command, std::string *output) {
  FILE *pipe = popen(command.c_str(), "r");
  if (!pipe) return false;
  char buffer[4096];
  size_t count;
  while ((count = std::fread(buffer, 1, sizeof(buffer), pipe)) != 0) {
    if (output->size() + count > 1024 * 1024) {
      pclose(pipe);
      return false;
    }
    output->append(buffer, count);
  }
  return pclose(pipe) == 0;
}

bool write_path_profile(const std::string &profile, const std::string &shell,
                        const std::string &directory) {
  std::string existing;
  const bool exists = read_text_file(profile, &existing);
  const std::string snippet = path_profile_snippet(shell, directory);
  if (exists && existing.find(snippet) != std::string::npos)
    return true;
  const size_t separator = profile.find_last_of(IsWindows() ? "\\/" : "/");
  if (separator != std::string::npos && !ensure_directory(profile.substr(0, separator)))
    return false;
  FILE *file = std::fopen(profile.c_str(), "ab");
  if (!file) return false;
  std::string addition;
  if (!existing.empty() && existing.back() != '\n') addition.push_back('\n');
  addition += snippet;
  const bool written = std::fwrite(addition.data(), 1, addition.size(), file) == addition.size();
  const bool closed = std::fclose(file) == 0;
  return written && closed;
}

std::string powershell_quote(const std::string &value) {
  std::string result = "'";
  for (char c : value) {
    result.push_back(c);
    if (c == '\'') result.push_back('\'');
  }
  result.push_back('\'');
  return result;
}

bool write_path_profiles(const InstallPlan &plan) {
  for (const auto &profile : plan.profiles)
    if (!write_path_profile(profile, plan.shell, plan.directory)) return false;
  return true;
}

bool update_windows_user_path(const InstallPlan &plan) {
  const std::string script = "$d=" + powershell_quote(plan.directory) + ";"
      "$p=[Environment]::GetEnvironmentVariable('Path','User');"
      "$items=@($p -split ';'|Where-Object {$_ -and $_.TrimEnd('\\') -ine $d.TrimEnd('\\')});"
      "[Environment]::SetEnvironmentVariable('Path',(@($d)+$items -join ';'),'User')";
  const std::string command =
      "powershell.exe -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass "
      "-EncodedCommand " + base64_utf16le_ascii(script);
  return std::system(command.c_str()) == 0;
}

bool confirm_install(const Release &release, const InstallPlan &plan,
                     bool assume_yes) {
  std::printf("\nInstall clud %s for this user?\n", release.version.c_str());
  std::printf("  Asset:      %s\n", release.filename.c_str());
  std::printf("  Download:   %s\n", release.url.c_str());
  std::printf("  Destination: %s\n", plan.executable.c_str());
  if (plan.path_update) {
    if (IsWindows()) std::puts("  PATH:       prepend this directory to the current user's PATH");
    else {
      std::puts("  PATH:       update shell startup files:");
      for (const auto &profile : plan.profiles) std::printf("              %s\n", profile.c_str());
    }
  } else {
    std::printf("  PATH:       %s is already first\n", plan.directory.c_str());
  }
  if (!plan.existing_clud.empty() && !path_equal(plan.existing_clud, plan.executable))
    std::printf("  Existing clud on PATH: %s (the install path will take precedence)\n",
                plan.existing_clud.c_str());
  if (assume_yes) {
    std::puts("  Consent:    accepted with --yes");
    return true;
  }
  if (!isatty(STDIN_FILENO)) {
    std::fputs("clud-installer: installation requires a terminal or --yes\n", stderr);
    return false;
  }
  std::fputs("Proceed? [y/N] ", stdout);
  std::fflush(stdout);
  char answer[32];
  if (!std::fgets(answer, sizeof(answer), stdin)) return false;
  return answer[0] == 'y' || answer[0] == 'Y';
}

bool write_installed_binary(const InstallPlan &plan,
                            const std::vector<unsigned char> &bytes,
                            std::string *backup_path, std::string *error) {
  if (!ensure_directory(plan.directory)) {
    *error = "could not create the install directory";
    return false;
  }
  const std::string stage = path_join(
      plan.directory, ".clud-installer-" + std::to_string(getpid()) + ".tmp");
  if (path_exists(stage)) {
    *error = "a staging file already exists; remove it or retry the installation";
    return false;
  }
  FILE *file = std::fopen(stage.c_str(), "wb");
  if (!file) {
    *error = "could not create a staging file in the install directory";
    return false;
  }
  const bool wrote = bytes.empty() ||
      std::fwrite(bytes.data(), 1, bytes.size(), file) == bytes.size();
  const bool flushed = std::fflush(file) == 0;
  const bool closed = std::fclose(file) == 0;
  if (!wrote || !flushed || !closed) {
    std::remove(stage.c_str());
    *error = "could not write the clud executable completely";
    return false;
  }
  if (!IsWindows() && chmod(stage.c_str(), 0755) != 0) {
    std::remove(stage.c_str());
    *error = "could not make the installed clud executable runnable";
    return false;
  }
  if (path_exists(plan.executable)) {
    for (unsigned attempt = 0; attempt < 20; ++attempt) {
      const std::string candidate = plan.executable + ".backup-" +
                                    std::to_string(getpid()) + "-" +
                                    std::to_string(attempt);
      if (path_exists(candidate)) continue;
      if (std::rename(plan.executable.c_str(), candidate.c_str()) == 0) {
        *backup_path = candidate;
        break;
      }
      std::remove(stage.c_str());
      *error = "could not preserve the previous clud executable";
      return false;
    }
    if (backup_path->empty()) {
      std::remove(stage.c_str());
      *error = "could not choose a safe backup name for the existing executable";
      return false;
    }
  }
  if (std::rename(stage.c_str(), plan.executable.c_str()) != 0) {
    if (!backup_path->empty()) std::rename(backup_path->c_str(), plan.executable.c_str());
    std::remove(stage.c_str());
    *error = "could not move the verified clud executable into place";
    return false;
  }
  return true;
}

bool verify_fresh_shell(const Release &release, const InstallPlan &plan,
                        std::string *output) {
  std::string command;
  if (IsWindows()) {
    std::string script = "$d=" + powershell_quote(plan.directory) + ";"
                         "$e=" + powershell_quote(plan.executable) + ";";
    if (plan.path_update) {
      script += "$u=[Environment]::GetEnvironmentVariable('Path','User');"
                "if(-not (($u -split ';')|Where-Object {$_ -and $_.TrimEnd('\\') -ieq $d.TrimEnd('\\')})){exit 3};";
    }
    script += "$env:Path=\"$d;$env:Path\";"
              "$c=Get-Command clud -CommandType Application -ErrorAction Stop;"
              "if(($c.Source -replace '/','\\').ToLowerInvariant() -ne ($e -replace '/','\\').ToLowerInvariant()){exit 4};"
              "$c.Source;& $c.Source --version";
    command = "powershell.exe -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass "
              "-EncodedCommand " + base64_utf16le_ascii(script);
  } else {
    const std::string check = "command -v clud; clud --version";
    const std::vector<std::string> modes = plan.verify_interactive_shell
        ? std::vector<std::string>{"-l", "-i"} : std::vector<std::string>{"-l"};
    const std::string baseline_path = "/usr/bin:/bin:/usr/sbin:/sbin";
    for (const auto &mode : modes) {
      std::string shell_output;
      command = "env -u BASH_ENV PATH=" + shell_quote(baseline_path) + " " +
                shell_quote(plan.shell) + " " + mode + " -c " + shell_quote(check);
      if (!run_capture(command, &shell_output)) {
        output->append(shell_output);
        return false;
      }
      output->append(shell_output);
      if (shell_output.find(plan.executable) == std::string::npos ||
          shell_output.find(release.version) == std::string::npos)
        return false;
    }
    return true;
  }
  if (!run_capture(command, output)) return false;
  std::string expected = plan.executable;
  if (IsWindows()) {
    for (char &c : expected) {
      if (c == '/') c = '\\';
      c = static_cast<char>(std::tolower(static_cast<unsigned char>(c)));
    }
    std::string actual = *output;
    for (char &c : actual) {
      if (c == '/') c = '\\';
      c = static_cast<char>(std::tolower(static_cast<unsigned char>(c)));
    }
    return actual.find(expected) != std::string::npos &&
           output->find(release.version) != std::string::npos;
  }
  return output->find(plan.executable) != std::string::npos &&
         output->find(release.version) != std::string::npos;
}

bool fetch_manifest(std::string *text);
bool load_catalog_text(const std::string &text, const Platform &platform,
                       std::vector<Release> *releases, std::string *latest,
                       std::string *error);
bool select_version(const std::vector<Release> &releases, const std::string &latest,
                    size_t *selected);

void remove_temp_directory(const std::string &directory,
                           const std::string &asset_path) {
  if (!asset_path.empty()) std::remove(asset_path.c_str());
  if (!directory.empty()) rmdir(directory.c_str());
}

int install_release(const Release &release, const Platform &platform,
                    const InstallPlan &plan, bool assume_yes) {
  if (!confirm_install(release, plan, assume_yes)) {
    std::puts("Installation cancelled; no files were changed.");
    return 1;
  }
  std::string temp_directory;
  if (!make_temp_directory(&temp_directory)) {
    std::fputs("clud-installer: could not create a private temporary directory\n", stderr);
    return 1;
  }
  const std::string asset_path = path_join(temp_directory, "release-asset");
  if (!download_asset(release, asset_path)) {
    remove_temp_directory(temp_directory, asset_path);
    std::fputs("clud-installer: release asset download failed\n", stderr);
    return 1;
  }
  uint64_t actual_size = 0;
  std::string actual_sha;
  if (!hash_file(asset_path, &actual_size, &actual_sha) ||
      actual_size != release.size || actual_sha != release.sha256) {
    remove_temp_directory(temp_directory, asset_path);
    std::fputs("clud-installer: downloaded asset size or SHA-256 does not match the catalog\n",
               stderr);
    return 1;
  }
  std::printf("Verified release asset: %llu bytes, SHA-256 %s\n",
              static_cast<unsigned long long>(actual_size), actual_sha.c_str());
  std::vector<unsigned char> asset;
  if (!read_binary_file(asset_path, &asset)) {
    remove_temp_directory(temp_directory, asset_path);
    std::fputs("clud-installer: verified release asset could not be read\n", stderr);
    return 1;
  }
  std::vector<unsigned char> executable;
  std::string error;
  if (release.media_type == "application/octet-stream") {
    executable = std::move(asset);
    if (!executable_bytes(executable, platform))
      error = "downloaded release asset is not an executable for this platform";
  } else if (release.media_type == "application/zip") {
    if (!extract_wheel_executable(asset, release, platform, &executable, &error)) {
      // error contains the extraction diagnosis.
    }
  } else {
    error = "the catalog selected an unsupported release asset media type";
  }
  remove_temp_directory(temp_directory, asset_path);
  if (!error.empty()) {
    std::fprintf(stderr, "clud-installer: %s\n", error.c_str());
    return 1;
  }
  std::string backup_path;
  if (!write_installed_binary(plan, executable, &backup_path, &error)) {
    std::fprintf(stderr, "clud-installer: %s\n", error.c_str());
    return 1;
  }
  if (plan.path_update && !(IsWindows() ? update_windows_user_path(plan)
                                        : write_path_profiles(plan))) {
    std::fprintf(stderr, "clud-installer: installed %s but could not update PATH\n",
                 plan.executable.c_str());
    return 1;
  }
  std::string verification;
  if (!verify_fresh_shell(release, plan, &verification)) {
    std::fprintf(stderr, "clud-installer: installed %s but a fresh shell did not resolve "
                         "the selected version\n%s\n",
                 plan.executable.c_str(), verification.c_str());
    if (!IsWindows())
      std::printf("Activate in this terminal with: export PATH=\"%s:$PATH\"\n",
                  plan.directory.c_str());
    return 1;
  }
  std::printf("Installed clud %s at %s\n", release.version.c_str(),
              plan.executable.c_str());
  std::printf("Fresh-shell verification: clud --version -> %s\n", release.version.c_str());
  if (plan.path_update) std::puts("Open a new terminal for the PATH change to take effect.");
  if (!backup_path.empty())
    std::printf("Previous executable preserved at %s\n", backup_path.c_str());
  return 0;
}

int run_installer(const char *requested_version, bool assume_yes) {
  Platform platform;
  if (!host_platform(&platform)) return 1;
  std::string catalog_text, latest, error;
  if (!fetch_manifest(&catalog_text)) return 1;
  std::vector<Release> releases;
  if (!load_catalog_text(catalog_text, platform, &releases, &latest, &error)) {
    std::fprintf(stderr, "clud-installer: invalid catalog: %s\n", error.c_str());
    return 1;
  }
  size_t selected = releases.size();
  if (requested_version) {
    for (size_t i = 0; i < releases.size(); ++i)
      if (releases[i].version == requested_version) selected = i;
    if (selected == releases.size()) {
      std::fprintf(stderr, "clud-installer: version %s is not installable for %s %s\n",
                   requested_version, platform.os.c_str(), platform.arch.c_str());
      return 1;
    }
  } else {
    if (assume_yes) {
      std::fputs("clud-installer: --yes requires --install-version VERSION\n", stderr);
      return 1;
    }
    if (!select_version(releases, latest, &selected)) return 1;
  }
  InstallPlan plan;
  if (!make_install_plan(platform, &plan, &error)) {
    std::fprintf(stderr, "clud-installer: %s\n", error.c_str());
    return 1;
  }
  return install_release(releases[selected], platform, plan, assume_yes);
}

bool fetch_manifest(std::string *text) {
  const std::string quote = IsWindows() ? "\"" : "'";
  const std::string curl = IsWindows() ? "curl.exe" : "curl";
  const std::string scheme = IsWindows() ? "\"=https\"" : "'=https'";
  const std::string command = curl + " -fLsS --proto " + scheme + " --tlsv1.2 " +
                              quote + kManifestUrl + quote;
  FILE *pipe = popen(command.c_str(), "r");
  if (!pipe) {
    std::fputs("clud-installer: could not start curl; install curl and try again\n", stderr);
    return false;
  }
  char buffer[8192];
  size_t count;
  while ((count = std::fread(buffer, 1, sizeof(buffer), pipe)) != 0) {
    if (text->size() + count > 5 * 1024 * 1024) {
      pclose(pipe);
      std::fputs("clud-installer: the release catalog is unexpectedly large\n", stderr);
      return false;
    }
    text->append(buffer, count);
  }
  const int status = pclose(pipe);
  if (status != 0 || text->empty()) {
    std::fputs("clud-installer: could not download the release catalog\n", stderr);
    return false;
  }
  return true;
}

bool load_catalog_text(const std::string &text, const Platform &platform,
                       std::vector<Release> *releases, std::string *latest,
                       std::string *error) {
  clud_installer::Json catalog;
  if (!clud_installer::JsonParser(text).parse(&catalog, error)) return false;
  return load_releases(catalog, platform, releases, latest, error);
}

class RawTerminal {
 public:
  RawTerminal() = default;
  RawTerminal(const RawTerminal &) = delete;
  RawTerminal &operator=(const RawTerminal &) = delete;
  ~RawTerminal() { restore(); }

  bool enter() {
    if (!isatty(STDIN_FILENO) || !isatty(STDOUT_FILENO)) return false;
    windows_ = IsWindows();
    if (windows_) {
      input_ = GetStdHandle(kNtStdInputHandle);
      output_ = GetStdHandle(kNtStdOutputHandle);
      if (!GetConsoleMode(input_, &input_mode_) || !GetConsoleMode(output_, &output_mode_))
        return false;
      const uint32_t new_input =
          (input_mode_ & ~(kNtEnableLineInput | kNtEnableEchoInput | kNtEnableProcessedInput)) |
          kNtEnableVirtualTerminalInput;
      const uint32_t new_output = output_mode_ | kNtEnableVirtualTerminalProcessing;
      if (!SetConsoleMode(input_, new_input)) return false;
      active_ = true;
      if (!SetConsoleMode(output_, new_output)) {
        restore();
        return false;
      }
      FlushConsoleInputBuffer(input_);
      return true;
    }
    if (tcgetattr(STDIN_FILENO, &original_) != 0) return false;
    termios raw = original_;
    raw.c_lflag &= static_cast<tcflag_t>(~(ICANON | ECHO | ISIG));
    raw.c_iflag &= static_cast<tcflag_t>(~(IXON | ICRNL));
    raw.c_oflag &= static_cast<tcflag_t>(~OPOST);
    raw.c_cc[VMIN] = 1;
    raw.c_cc[VTIME] = 0;
    if (tcsetattr(STDIN_FILENO, TCSANOW, &raw) != 0) return false;
    tcflush(STDIN_FILENO, TCIFLUSH);
    active_ = true;
    return true;
  }

  void restore() {
    if (!active_) return;
    if (windows_) {
      SetConsoleMode(input_, input_mode_);
      SetConsoleMode(output_, output_mode_);
    } else {
      tcsetattr(STDIN_FILENO, TCSANOW, &original_);
    }
    active_ = false;
  }

  int read_byte() {
    if (windows_) {
      char byte;
      uint32_t count = 0;
      if (!ReadFile(input_, &byte, 1, &count, nullptr) || count != 1) return -1;
      return static_cast<unsigned char>(byte);
    }
    unsigned char byte;
    if (read(STDIN_FILENO, &byte, 1) != 1) return -1;
    return byte;
  }

  int read_byte_timeout(int milliseconds) {
    if (!windows_) {
      struct pollfd fd = {STDIN_FILENO, POLLIN, 0};
      const int ready = poll(&fd, 1, milliseconds);
      if (ready <= 0) return -1;
    } else {
      for (int waited = 0; waited < milliseconds; waited += 5) {
        uint32_t events = 0;
        if (!GetNumberOfConsoleInputEvents(input_, &events)) return -1;
        if (events) break;
        usleep(5000);
      }
      uint32_t events = 0;
      if (!GetNumberOfConsoleInputEvents(input_, &events) || events == 0) return -1;
    }
    return read_byte();
  }

 private:
  bool active_ = false;
  bool windows_ = false;
  int64_t input_ = -1;
  int64_t output_ = -1;
  uint32_t input_mode_ = 0;
  uint32_t output_mode_ = 0;
  termios original_{};
};

bool write_terminal(const std::string &bytes) {
  size_t offset = 0;
  while (offset < bytes.size()) {
    const ssize_t written = write(STDOUT_FILENO, bytes.data() + offset, bytes.size() - offset);
    if (written <= 0) return false;
    offset += static_cast<size_t>(written);
  }
  return true;
}

int read_menu_key(RawTerminal *terminal) {
  const int key = terminal->read_byte();
  if (key != 0x1b) return key;
  const int bracket = terminal->read_byte_timeout(80);
  if (bracket != '[' && bracket != 'O') return 0x1b;
  const int arrow = terminal->read_byte();
  if (arrow == 'A') return 0x100;  // Up.
  if (arrow == 'B') return 0x101;  // Down.
  return 0;
}

bool select_version(const std::vector<Release> &releases, const std::string &latest,
                    size_t *selected) {
  size_t current = 0;
  for (size_t i = 0; i < releases.size(); ++i)
    if (releases[i].version == latest) current = i;
  if (!isatty(STDIN_FILENO) || !isatty(STDOUT_FILENO)) {
    std::fputs("clud-installer: no interactive terminal; use --install-version VERSION --yes\n",
               stderr);
    return false;
  }
  std::vector<std::string> versions;
  for (const auto &release : releases) versions.push_back(release.version);
  versions[current] = "latest (" + versions[current] + ")";
  RawTerminal terminal;
  if (!terminal.enter()) {
    std::fputs("clud-installer: could not enter terminal input mode\n", stderr);
    return false;
  }
  constexpr size_t kVisibleVersions = 8;
  std::string frame = clud_installer::menu_frame("Choose a clud version (latest selected by default)",
                                                  versions.data(), versions.size(), current,
                                                  kVisibleVersions);
  size_t frame_rows = static_cast<size_t>(std::count(frame.begin(), frame.end(), '\n'));
  write_terminal("\x1b[?25l" + frame);
  while (true) {
    const int key = read_menu_key(&terminal);
    if (key == '\r' || key == '\n') {
      *selected = current;
      break;
    }
    if (key == 0x1b || key == 3 || key == 4 || key == -1) {
      terminal.restore();
      write_terminal("\x1b[?25h\r\nInstallation cancelled.\r\n");
      return false;
    }
    if (key == 0x100 || key == 'k') {
      if (current > 0) --current;
      else continue;
    } else if (key == 0x101 || key == 'j') {
      if (current + 1 < releases.size()) ++current;
      else continue;
    } else {
      continue;
    }
    frame = clud_installer::menu_frame("Choose a clud version (latest selected by default)",
                                       versions.data(), versions.size(), current,
                                       kVisibleVersions);
    const size_t next_rows = static_cast<size_t>(std::count(frame.begin(), frame.end(), '\n'));
    std::string redraw = "\r\x1b[" + std::to_string(frame_rows) + "A\x1b[J" + frame;
    write_terminal(redraw);
    frame_rows = next_rows;
  }
  terminal.restore();
  write_terminal("\x1b[?25h\r\n");
  return true;
}

bool load_releases(const clud_installer::Json &catalog, const Platform &platform,
                   std::vector<Release> *result, std::string *latest_stable,
                   std::string *error) {
  std::string kind, tool;
  if (!get_string(&catalog, "kind", &kind) || kind != "Catalog" ||
      !get_string(&catalog, "tool", &tool) || tool != "clud") {
    *error = "the manifest is not a clud Catalog";
    return false;
  }
  const auto *channels = catalog.get("channels");
  if (!get_string(channels, "latest-stable", latest_stable)) {
    *error = "the manifest has no latest-stable channel";
    return false;
  }
  const auto *entries = catalog.get("releases");
  if (!entries || entries->type != clud_installer::Json::Type::Array) {
    *error = "the manifest has no releases list";
    return false;
  }
  for (const auto &entry : entries->array) {
    Release release;
    if (!get_string(&entry, "version", &release.version)) continue;
    const auto *platforms = entry.get("platforms");
    if (!platforms || platforms->type != clud_installer::Json::Type::Array) continue;
    for (const auto &candidate : platforms->array) {
      std::string os, arch;
      const auto *target = candidate.get("platform");
      if (!get_string(target, "os", &os) || !get_string(target, "arch", &arch) ||
          os != platform.os || arch != platform.arch)
        continue;
      const auto *asset = candidate.get("asset");
      const auto *urls = asset ? asset->get("urls") : nullptr;
      if (!get_string(asset, "filename", &release.filename) ||
          !get_string(asset, "sha256", &release.sha256) ||
          !get_string(asset, "media_type", &release.media_type) ||
          !valid_sha256(release.sha256) || !urls ||
          urls->type != clud_installer::Json::Type::Array || urls->array.empty() ||
          !urls->array[0].string()) {
        *error = "a platform entry is missing a valid filename, digest, media type or URL";
        return false;
      }
      release.url = *urls->array[0].string();
      if (!safe_asset_url(release.url) || !parse_uint64(asset->get("size_bytes"), &release.size)) {
        *error = "a platform entry has an unsafe URL or invalid asset size";
        return false;
      }
      result->push_back(release);
      break;
    }
  }
  if (result->empty()) {
    *error = "the catalog has no releases for this OS and architecture";
    return false;
  }
  return true;
}

}  // namespace

static int test_json() {
  const std::string sample =
      R"({"kind":"Catalog","channels":{"latest-stable":"2.10.0"},"releases":[{"version":"2.10.0","n":4,"ok":true,"label":"caf\u00e9"}]})";
  clud_installer::Json root;
  std::string error;
  if (!clud_installer::JsonParser(sample).parse(&root, &error)) {
    std::fprintf(stderr, "JSON self-test parse failed: %s\n", error.c_str());
    return 1;
  }
  const auto *kind = root.get("kind");
  const auto *channels = root.get("channels");
  const auto *latest = channels ? channels->get("latest-stable") : nullptr;
  const auto *releases = root.get("releases");
  const auto *label = releases && !releases->array.empty()
                          ? releases->array[0].get("label")
                          : nullptr;
  if (!kind || !kind->string() || *kind->string() != "Catalog" ||
      !latest || !latest->string() || *latest->string() != "2.10.0" ||
      !releases || releases->array.size() != 1 || !label || !label->string() ||
      *label->string() != "caf\xc3\xa9") {
    std::fprintf(stderr, "JSON self-test returned unexpected values\n");
    return 1;
  }
  for (const std::string bad : {"{", "[1,]", "{\"x\":1,\"x\":2}", "true false"}) {
    clud_installer::Json ignored;
    if (clud_installer::JsonParser(bad).parse(&ignored, &error)) {
      std::fprintf(stderr, "JSON self-test accepted invalid input: %s\n", bad.c_str());
      return 1;
    }
  }
  std::puts("installer JSON tests passed");
  return 0;
}

static int test_catalog() {
  const std::string sample = R"({
    "kind":"Catalog","tool":"clud",
    "channels":{"latest-stable":"2.10.0"},
    "releases":[
      {"version":"2.10.0","platforms":[
        {"platform":{"os":"linux","arch":"x86_64"},
         "asset":{"filename":"clud-2.10.0-x86_64-unknown-linux-gnu",
                  "media_type":"application/octet-stream","size_bytes":1234,
                  "sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                  "urls":["https://github.com/zackees/clud/releases/download/2.10.0/clud"]}},
        {"platform":{"os":"linux","arch":"aarch64"},
         "asset":{"filename":"arm","media_type":"application/octet-stream","size_bytes":10,
                  "sha256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                  "urls":["https://github.com/zackees/clud/releases/download/2.10.0/arm"]}}
      ]}
    ]})";
  clud_installer::Json catalog;
  std::string error;
  if (!clud_installer::JsonParser(sample).parse(&catalog, &error)) {
    std::fprintf(stderr, "catalog test parse failed: %s\n", error.c_str());
    return 1;
  }
  std::vector<Release> releases;
  std::string latest;
  if (!load_releases(catalog, Platform{"linux", "x86_64"}, &releases,
                     &latest, &error)) {
    std::fprintf(stderr, "catalog selection failed: %s\n", error.c_str());
    return 1;
  }
  if (latest != "2.10.0" || releases.size() != 1 ||
      releases[0].filename != "clud-2.10.0-x86_64-unknown-linux-gnu" ||
      releases[0].size != 1234 || releases[0].sha256[0] != 'a') {
    std::fputs("catalog selected unexpected platform asset\n", stderr);
    return 1;
  }
  std::puts("installer catalog tests passed");
  return 0;
}

static int test_shell_profiles() {
  const std::string home = "/home/example";
  const auto bash_default = shell_profiles(home, "/bin/bash", false, false);
  const auto bash_profile = shell_profiles(home, "/bin/bash", true, true);
  const auto bash_login = shell_profiles(home, "/bin/bash", false, true);
  const auto zsh = shell_profiles(home, "/bin/zsh", false, false);
  const auto fish = shell_profiles(home, "/usr/bin/fish", false, false);
  const auto sh = shell_profiles(home, "/bin/sh", false, false);
  if (bash_default != std::vector<std::string>{"/home/example/.profile",
                                               "/home/example/.bashrc"} ||
      bash_profile != std::vector<std::string>{"/home/example/.bash_profile",
                                               "/home/example/.bashrc"} ||
      bash_login != std::vector<std::string>{"/home/example/.bash_login",
                                             "/home/example/.bashrc"} ||
      zsh != std::vector<std::string>{"/home/example/.zprofile",
                                     "/home/example/.zshrc"} ||
      fish != std::vector<std::string>{
          "/home/example/.config/fish/conf.d/clud-path.fish"} ||
      sh != std::vector<std::string>{"/home/example/.profile"} ||
      default_shell(Platform{"darwin", "aarch64"}) != "/bin/zsh" ||
      default_shell(Platform{"linux", "x86_64"}) != "/bin/bash" ||
      default_shell(Platform{"linux", "x86_64"}, "/usr/bin/fish") !=
          "/usr/bin/fish") {
    std::fputs("shell startup profile selection failed\n", stderr);
    return 1;
  }
  std::puts("installer shell PATH profile tests passed");
  return 0;
}

static int test_tty_renderer() {
  const std::string normalized =
      clud_installer::safe_crlf("lf\ncrlf\r\ncr\rcontrol\t\x1b[31m");
  if (normalized != "lf\r\ncrlf\r\ncrcontrol[31m" ||
      !clud_installer::valid_crlf_stream(normalized)) {
    std::fputs("TTY renderer failed CRLF/control-character checks\n", stderr);
    return 1;
  }
  const auto first = clud_installer::menu_window(0, 25, 8);
  const auto middle = clud_installer::menu_window(12, 25, 8);
  const auto last = clud_installer::menu_window(24, 25, 8);
  if (first.begin != 0 || first.end != 8 || middle.begin != 5 || middle.end != 13 ||
      last.begin != 17 || last.end != 25) {
    std::fputs("TTY renderer failed scrolling-window checks\n", stderr);
    return 1;
  }
  std::string versions[3] = {"2.8.14", "2.8.13", "2.8.12"};
  const std::string frame = clud_installer::menu_frame("Select version", versions, 3, 0, 8);
  if (frame.find("> [*] 2.8.14\r\n") == std::string::npos ||
      frame.find("  [ ] 2.8.13\r\n") == std::string::npos ||
      !clud_installer::valid_crlf_stream(frame)) {
    std::fputs("TTY renderer failed selection/default-marker checks\n", stderr);
    return 1;
  }
  std::puts("installer TTY renderer tests passed");
  return 0;
}

static int test_sha256() {
  clud_installer::Sha256 digest;
  digest.update("abc", 3);
  if (digest.finish() !=
      "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad") {
    std::fputs("SHA-256 implementation failed its known-answer test\n", stderr);
    return 1;
  }
  std::puts("installer SHA-256 tests passed");
  return 0;
}

static int list_versions(const char *catalog_path = nullptr) {
  Platform platform;
  if (!host_platform(&platform)) return 1;
  std::string text, latest, error;
  std::vector<Release> releases;
  const bool read_ok = catalog_path ? read_text_file(catalog_path, &text)
                                    : fetch_manifest(&text);
  if (!read_ok || !load_catalog_text(text, platform, &releases, &latest, &error)) {
    if (!error.empty()) std::fprintf(stderr, "clud-installer: invalid catalog: %s\n", error.c_str());
    else std::fputs("clud-installer: could not read the catalog\n", stderr);
    return 1;
  }
  std::printf("Available clud versions for %s %s:\n", platform.os.c_str(), platform.arch.c_str());
  for (const auto &release : releases)
    std::printf("%s %s\n", release.version == latest ? "[*]" : "[ ]", release.version.c_str());
  return 0;
}

static int print_latest_stable(const char *catalog_path = nullptr) {
  Platform platform;
  if (!host_platform(&platform)) return 1;
  std::string text, latest, error;
  std::vector<Release> releases;
  const bool read_ok = catalog_path ? read_text_file(catalog_path, &text)
                                    : fetch_manifest(&text);
  if (!read_ok || !load_catalog_text(text, platform, &releases, &latest, &error)) {
    if (!error.empty()) std::fprintf(stderr, "clud-installer: invalid catalog: %s\n", error.c_str());
    else std::fputs("clud-installer: could not read the catalog\n", stderr);
    return 1;
  }
  for (const auto &release : releases) {
    if (release.version == latest) {
      std::puts(release.version.c_str());
      return 0;
    }
  }
  std::fprintf(stderr, "clud-installer: latest-stable %s is unavailable for %s %s\n",
               latest.c_str(), platform.os.c_str(), platform.arch.c_str());
  return 1;
}

int main(int argc, char **argv) {
  if (argc == 1) return run_installer(nullptr, false);
  if (argc == 2 && std::strcmp(argv[1], "--version") == 0) {
    std::printf("clud-installer 0.1.0\n");
    return 0;
  }
  if (argc == 2 && std::strcmp(argv[1], "--self-test-json") == 0) {
    return test_json();
  }
  if (argc == 2 && std::strcmp(argv[1], "--self-test-catalog") == 0) {
    return test_catalog();
  }
  if (argc == 2 && std::strcmp(argv[1], "--self-test-tty") == 0) {
    return test_tty_renderer();
  }
  if (argc == 2 && std::strcmp(argv[1], "--self-test-sha256") == 0) {
    return test_sha256();
  }
  if (argc == 2 && std::strcmp(argv[1], "--self-test-path") == 0) {
    return test_shell_profiles();
  }
  if (argc == 2 && std::strcmp(argv[1], "--host-platform") == 0) {
    Platform platform;
    if (!host_platform(&platform)) return 1;
    std::printf("%s %s\n", platform.os.c_str(), platform.arch.c_str());
    return 0;
  }
  if (argc == 2 && std::strcmp(argv[1], "--list") == 0) {
    return list_versions();
  }
  if (argc == 2 && std::strcmp(argv[1], "--latest-stable") == 0) {
    return print_latest_stable();
  }
  if (argc == 3 && std::strcmp(argv[1], "--list-from") == 0) {
    return list_versions(argv[2]);
  }
  const char *requested = nullptr;
  bool assume_yes = false;
  for (int i = 1; i < argc; ++i) {
    if (std::strcmp(argv[i], "--install-version") == 0 && i + 1 < argc) {
      if (requested) {
        std::fputs("clud-installer: specify --install-version only once\n", stderr);
        return 2;
      }
      requested = argv[++i];
    } else if (std::strcmp(argv[i], "--yes") == 0) {
      assume_yes = true;
    } else {
      std::fprintf(stderr, "clud-installer: unknown or incomplete argument: %s\n", argv[i]);
      return 2;
    }
  }
  if (!requested) {
    std::fputs("clud-installer: --yes requires --install-version VERSION\n", stderr);
    return 2;
  }
  return run_installer(requested, assume_yes);
}
