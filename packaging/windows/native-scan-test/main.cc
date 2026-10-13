#include <windows.h>
#include "scan.h"
#include <array>
#include <cstdio>
#include <cstdlib>
#include <vector>

extern "C" void seed_init(unsigned seed);

// No destructible C++ objects here: SEH contains faults from the isolated probe.
static bool initialize(unsigned seed, unsigned long* exception) {
  __try {
    seed_init(seed);
    return true;
  } __except (EXCEPTION_EXECUTE_HANDLER) {
    *exception = GetExceptionCode();
    return false;
  }
}

struct Coord { int x, y; };
static std::vector<Coord> expected_order(int side, int scan) {
  std::vector<Coord> result;
  for (int index = 0; index < side * side; ++index) {
    if (scan == 1) result.push_back({index % side, index / side});
    if (scan == 2) result.push_back({index / side, index % side});
  }
  if (scan == 0) {
    for (int sum = 0; sum <= 2 * (side - 1); ++sum) {
      for (int x = 0; x < side; ++x) {
        const int y = sum - x;
        if (y >= 0 && y < side) result.push_back({x, y});
      }
    }
  }
  return result;
}

int main(int argc, char** argv) {
  SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX);
  const unsigned seed = argc == 2 ? static_cast<unsigned>(std::strtoul(argv[1], nullptr, 10)) : 0;
  unsigned long exception = 0;
  const bool initialized = initialize(seed, &exception);
  if (!initialized) {
    std::printf("{\"compiler\":%d,\"seed\":%u,\"initialized\":false,\"exception\":\"0x%08lx\",\"base_checked\":0,\"positions_checked\":0}\n",
                _MSC_VER, seed, exception);
    return 2;
  }

  int base_checked = 0, base_errors = 0, position_checked = 0, position_errors = 0, duplicates = 0;
  int reported = 0;
  for (int logsize = 0; logsize <= 5; ++logsize) {
    const int side = 1 << logsize;
    for (int scan = 0; scan < 3; ++scan) {
      const auto reference = expected_order(side, scan);
      const position* actual = get_scan_order(logsize, scan);
      for (int index = 0; index < side * side; ++index) {
        ++base_checked;
        if (actual[index].x != reference[index].x || actual[index].y != reference[index].y)
          ++base_errors;
      }
    }
  }

  for (int logsize = 2; logsize <= 5; ++logsize) {
    const int side = 1 << logsize;
    const int subside = side / 4;
    for (int scan = 0; scan < 3; ++scan) {
      const auto sub_order = expected_order(subside, scan);
      const auto pixel_order = expected_order(4, scan);
      std::vector<int> sub_inverse(subside * subside), pixel_inverse(16);
      for (int i = 0; i < subside * subside; ++i)
        sub_inverse[sub_order[i].y * subside + sub_order[i].x] = i;
      for (int i = 0; i < 16; ++i)
        pixel_inverse[pixel_order[i].y * 4 + pixel_order[i].x] = i;
      std::vector<bool> seen(side * side, false);
      for (int y = 0; y < side; ++y) {
        for (int x = 0; x < side; ++x) {
          ++position_checked;
          const scan_position actual = get_scan_position(x, y, scan, logsize);
          const int want_sub = sub_inverse[(y / 4) * subside + x / 4];
          const int want_pos = pixel_inverse[(y % 4) * 4 + x % 4];
          if (actual.subBlock != want_sub || actual.scanPos != want_pos) {
            ++position_errors;
            if (reported++ < 8) {
              std::fprintf(stderr, "position mismatch: log2=%d scan=%d x=%d y=%d actual=(%u,%u) expected=(%d,%d)\n",
                           logsize, scan, x, y, actual.subBlock, actual.scanPos, want_sub, want_pos);
            }
          }
          const int combined = actual.subBlock * 16 + actual.scanPos;
          if (actual.subBlock < subside * subside && actual.scanPos < 16) {
            if (seen[combined]) ++duplicates;
            seen[combined] = true;
          }
        }
      }
    }
  }

  std::printf("{\"compiler\":%d,\"seed\":%u,\"initialized\":true,\"base_checked\":%d,\"base_errors\":%d,\"positions_checked\":%d,\"position_errors\":%d,\"duplicate_positions\":%d}\n",
              _MSC_VER, seed, base_checked, base_errors, position_checked, position_errors, duplicates);
  return base_errors == 0 && position_errors == 0 && duplicates == 0 ? 0 : 1;
}

