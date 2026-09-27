#ifndef CLUD_INSTALLER_SHA256_HPP
#define CLUD_INSTALLER_SHA256_HPP

#include <cstddef>
#include <cstdint>
#include <string>

namespace clud_installer {

class Sha256 {
 public:
  void update(const void *input, size_t length) {
    const auto *bytes = static_cast<const uint8_t *>(input);
    bit_count_ += static_cast<uint64_t>(length) * 8;
    while (length) {
      const size_t take = length < sizeof(block_) - used_ ? length : sizeof(block_) - used_;
      for (size_t i = 0; i < take; ++i) block_[used_ + i] = bytes[i];
      used_ += take;
      bytes += take;
      length -= take;
      if (used_ == sizeof(block_)) {
        transform(block_);
        used_ = 0;
      }
    }
  }

  std::string finish() {
    const uint64_t bits = bit_count_;
    const uint8_t marker = 0x80;
    update(&marker, 1);
    const uint8_t zero = 0;
    while (used_ != 56) update(&zero, 1);
    uint8_t length[8];
    for (int i = 0; i < 8; ++i) length[7 - i] = static_cast<uint8_t>(bits >> (i * 8));
    update(length, sizeof(length));
    static constexpr char digits[] = "0123456789abcdef";
    std::string result;
    result.reserve(64);
    for (uint32_t word : state_)
      for (int byte = 3; byte >= 0; --byte) {
        const uint8_t value = static_cast<uint8_t>(word >> (byte * 8));
        result.push_back(digits[value >> 4]);
        result.push_back(digits[value & 0x0f]);
      }
    return result;
  }

 private:
  static uint32_t rotate(uint32_t value, unsigned bits) {
    return (value >> bits) | (value << (32 - bits));
  }

  void transform(const uint8_t block[64]) {
    static constexpr uint32_t constants[64] = {
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1,
        0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3,
        0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
        0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147,
        0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
        0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
        0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
        0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2};
    uint32_t words[64];
    for (int i = 0; i < 16; ++i) {
      const int at = i * 4;
      words[i] = (static_cast<uint32_t>(block[at]) << 24) |
                 (static_cast<uint32_t>(block[at + 1]) << 16) |
                 (static_cast<uint32_t>(block[at + 2]) << 8) | block[at + 3];
    }
    for (int i = 16; i < 64; ++i) {
      const uint32_t s0 = rotate(words[i - 15], 7) ^ rotate(words[i - 15], 18) ^
                          (words[i - 15] >> 3);
      const uint32_t s1 = rotate(words[i - 2], 17) ^ rotate(words[i - 2], 19) ^
                          (words[i - 2] >> 10);
      words[i] = words[i - 16] + s0 + words[i - 7] + s1;
    }
    uint32_t a = state_[0], b = state_[1], c = state_[2], d = state_[3];
    uint32_t e = state_[4], f = state_[5], g = state_[6], h = state_[7];
    for (int i = 0; i < 64; ++i) {
      const uint32_t sum1 = rotate(e, 6) ^ rotate(e, 11) ^ rotate(e, 25);
      const uint32_t choice = (e & f) ^ (~e & g);
      const uint32_t first = h + sum1 + choice + constants[i] + words[i];
      const uint32_t sum0 = rotate(a, 2) ^ rotate(a, 13) ^ rotate(a, 22);
      const uint32_t majority = (a & b) ^ (a & c) ^ (b & c);
      const uint32_t second = sum0 + majority;
      h = g; g = f; f = e; e = d + first;
      d = c; c = b; b = a; a = first + second;
    }
    state_[0] += a; state_[1] += b; state_[2] += c; state_[3] += d;
    state_[4] += e; state_[5] += f; state_[6] += g; state_[7] += h;
  }

  uint32_t state_[8] = {0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
                        0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19};
  uint8_t block_[64]{};
  size_t used_ = 0;
  uint64_t bit_count_ = 0;
};

}  // namespace clud_installer

#endif  // CLUD_INSTALLER_SHA256_HPP
