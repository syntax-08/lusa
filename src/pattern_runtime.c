/* Module 13: bounded Lua-style string pattern matching for the C11 runtime.
 *
 * The generated translation unit declares the entry points below before user
 * code and appends these definitions through `lusa::cbackend`.  The matcher
 * mirrors `src/pattern.rs`: both operate on Unicode scalar values, share the
 * same depth and step budgets, and use the same replacement grammar. */

#define LUSA_PAT_MAX_CAPTURES 32u
#define LUSA_PAT_MAX_DEPTH 200
#define LUSA_PAT_MAX_STEPS 1000000ull

#define LUSA_PAT_OPEN 0u
#define LUSA_PAT_SUBSTRING 1u
#define LUSA_PAT_POSITION 2u

typedef struct LusaPatCapture {
    uint64_t start;
    uint64_t end;
    uint32_t kind;
} LusaPatCapture;

typedef struct LusaMatcher {
    const uint32_t *src;
    uint64_t src_len;
    const uint32_t *pat;
    uint64_t pat_len;
    LusaPatCapture caps[LUSA_PAT_MAX_CAPTURES];
    int32_t level;
    uint64_t steps;
    bool failed;
    char error[192];
} LusaMatcher;

typedef struct LusaPatResult {
    bool found;
    uint64_t start;
    uint64_t end;
    int32_t capture_count;
    LusaPatCapture captures[LUSA_PAT_MAX_CAPTURES];
} LusaPatResult;

typedef struct LusaGmatch {
    LusaString *source;
    uint32_t *src;
    uint64_t *offsets;
    uint64_t src_len;
    uint32_t *pat;
    uint64_t pat_len;
    uint64_t pos;
    bool done;
} LusaGmatch;

static void lusa_drop_gmatch(LusaGmatch **value);

static void lusa_pat_set_error(LusaMatcher *ms, const char *message) {
    if (!ms->failed) {
        ms->failed = true;
        snprintf(ms->error, sizeof(ms->error), "%s", message);
    }
}

static bool lusa_pat_utf8_next(const char *data, uint64_t len, uint64_t *index,
                               uint32_t *codepoint) {
    unsigned char first;
    uint32_t value;
    uint64_t i;
    int extra;
    if (*index >= len) return false;
    first = (unsigned char)data[*index];
    if (first < 0x80u) {
        *codepoint = first;
        *index = *index + 1u;
        return true;
    }
    if ((first & 0xe0u) == 0xc0u) {
        value = (uint32_t)(first & 0x1fu);
        extra = 1;
    } else if ((first & 0xf0u) == 0xe0u) {
        value = (uint32_t)(first & 0x0fu);
        extra = 2;
    } else if ((first & 0xf8u) == 0xf0u) {
        value = (uint32_t)(first & 0x07u);
        extra = 3;
    } else {
        *codepoint = first;
        *index = *index + 1u;
        return true;
    }
    if (*index + (uint64_t)extra >= len) {
        *codepoint = first;
        *index = *index + 1u;
        return true;
    }
    i = 1u;
    while (i <= (uint64_t)extra) {
        unsigned char next = (unsigned char)data[*index + i];
        if ((next & 0xc0u) != 0x80u) {
            *codepoint = first;
            *index = *index + 1u;
            return true;
        }
        value = (value << 6) | (uint32_t)(next & 0x3fu);
        i += 1u;
    }
    *codepoint = value;
    *index = *index + 1u + (uint64_t)extra;
    return true;
}

static void lusa_pat_decode(LusaString *text, uint32_t **out_cps,
                            uint64_t **out_offsets, uint64_t *out_len) {
    uint64_t len = text == NULL ? 0u : text->obj.length;
    uint32_t *cps = (uint32_t *)malloc((size_t)(len + 1u) * sizeof(uint32_t));
    uint64_t *offsets = (uint64_t *)malloc((size_t)(len + 1u) * sizeof(uint64_t));
    uint64_t index = 0u;
    uint64_t count = 0u;
    if (cps == NULL || offsets == NULL) lusa_panic("out of memory");
    while (index < len) {
        offsets[count] = index;
        lusa_pat_utf8_next(text->data, len, &index, &cps[count]);
        count = count + 1u;
    }
    offsets[count] = len;
    *out_cps = cps;
    *out_offsets = offsets;
    *out_len = count;
}

static bool lusa_pat_iscntrl(uint32_t c) {
    return c < 32u || c == 127u;
}
static bool lusa_pat_isspace(uint32_t c) {
    return c == 32u || (c >= 9u && c <= 13u);
}
static bool lusa_pat_class(uint32_t c, uint32_t cls) {
    bool upper = cls >= 'A' && cls <= 'Z';
    uint32_t lower = upper ? (cls + 32u) : cls;
    bool matched;
    switch (lower) {
        case 'a': matched = (c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z'); break;
        case 'c': matched = lusa_pat_iscntrl(c); break;
        case 'd': matched = c >= '0' && c <= '9'; break;
        case 'g': matched = c >= 33u && c <= 126u; break;
        case 'l': matched = c >= 'a' && c <= 'z'; break;
        case 'p': matched = (c >= 33u && c <= 47u) || (c >= 58u && c <= 64u)
            || (c >= 91u && c <= 96u) || (c >= 123u && c <= 126u); break;
        case 's': matched = lusa_pat_isspace(c); break;
        case 'u': matched = c >= 'A' && c <= 'Z'; break;
        case 'w': matched = (c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z')
            || (c >= '0' && c <= '9'); break;
        case 'x': matched = (c >= '0' && c <= '9') || (c >= 'A' && c <= 'F')
            || (c >= 'a' && c <= 'f'); break;
        case 'z': matched = c == 0u; break;
        default: matched = c == lower; break;
    }
    return upper ? !matched : matched;
}

static bool lusa_pat_scan_set(const uint32_t *pat, uint64_t len, uint64_t start,
                              uint64_t *out_end, char *error) {
    uint64_t i = start + 1u;
    if (i < len && pat[i] == '^') i += 1u;
    if (i < len && pat[i] == ']') i += 1u;
    while (i < len) {
        if (pat[i] == ']') {
            *out_end = i + 1u;
            return true;
        }
        if (pat[i] == '%') {
            if (i + 1u >= len) {
                snprintf(error, 192, "%s", "malformed pattern: '%' at end of character set");
                return false;
            }
            i += 2u;
        } else {
            i += 1u;
        }
    }
    snprintf(error, 192, "%s", "malformed pattern: missing ']' in character set");
    return false;
}

static bool lusa_pat_validate(const uint32_t *pat, uint64_t len,
                              int32_t *capture_count, char *error) {
    uint64_t i = 0u;
    uint64_t open[LUSA_PAT_MAX_CAPTURES];
    int32_t open_count = 0;
    int32_t count = 0;
    while (i < len) {
        if (pat[i] == '%') {
            if (i + 1u >= len) {
                snprintf(error, 192, "%s", "malformed pattern: '%' at end of pattern");
                return false;
            }
            if (pat[i + 1u] == 'b') {
                if (i + 3u >= len) {
                    snprintf(error, 192, "%s", "malformed pattern: '%b' needs two characters");
                    return false;
                }
                i += 4u;
            } else if (pat[i + 1u] == 'f') {
                uint64_t end = 0u;
                if (i + 2u >= len || pat[i + 2u] != '[') {
                    snprintf(error, 192, "%s", "malformed pattern: '%f' must be followed by a set");
                    return false;
                }
                if (!lusa_pat_scan_set(pat, len, i + 2u, &end, error)) return false;
                i = end;
            } else if (pat[i + 1u] >= '0' && pat[i + 1u] <= '9') {
                int32_t index = (int32_t)(pat[i + 1u] - '0');
                if (index == 0 || index > count) {
                    snprintf(error, 192,
                        "malformed pattern: back reference %%%d has no earlier capture",
                        (int)index);
                    return false;
                }
                i += 2u;
            } else {
                i += 2u;
            }
        } else if (pat[i] == '[') {
            uint64_t end = 0u;
            if (!lusa_pat_scan_set(pat, len, i, &end, error)) return false;
            i = end;
        } else if (pat[i] == '(') {
            if (count >= (int32_t)LUSA_PAT_MAX_CAPTURES) {
                snprintf(error, 192, "%s", "malformed pattern: too many captures (maximum is 32)");
                return false;
            }
            count += 1;
            if (i + 1u < len && pat[i + 1u] == ')') {
                i += 2u;
            } else {
                open[open_count] = i;
                open_count += 1;
                i += 1u;
            }
        } else if (pat[i] == ')') {
            if (open_count == 0) {
                snprintf(error, 192, "%s", "malformed pattern: unmatched ')'");
                return false;
            }
            open_count -= 1;
            i += 1u;
        } else {
            i += 1u;
        }
    }
    if (open_count != 0) {
        snprintf(error, 192, "%s", "malformed pattern: unfinished capture");
        return false;
    }
    if (capture_count != NULL) *capture_count = count;
    return true;
}

static bool lusa_pat_tick(LusaMatcher *ms) {
    ms->steps += 1u;
    if (ms->steps > LUSA_PAT_MAX_STEPS) {
        lusa_pat_set_error(ms,
            "pattern exceeded the bounded matching budget; simplify nested quantifiers");
        return false;
    }
    return true;
}

static bool lusa_pat_class_end(LusaMatcher *ms, uint64_t p, uint64_t *out_end) {
    if (p >= ms->pat_len) {
        lusa_pat_set_error(ms, "malformed pattern: unexpected end of pattern");
        return false;
    }
    if (ms->pat[p] == '%') {
        if (p + 1u >= ms->pat_len) {
            lusa_pat_set_error(ms, "malformed pattern: '%' at end of pattern");
            return false;
        }
        if (ms->pat[p + 1u] == 'b') {
            if (p + 3u >= ms->pat_len) {
                lusa_pat_set_error(ms, "malformed pattern: '%b' needs two characters");
                return false;
            }
            *out_end = p + 4u;
            return true;
        }
        if (ms->pat[p + 1u] == 'f') {
            char error[192];
            uint64_t end = 0u;
            if (p + 2u >= ms->pat_len || ms->pat[p + 2u] != '[') {
                lusa_pat_set_error(ms, "malformed pattern: '%f' must be followed by a set");
                return false;
            }
            if (!lusa_pat_scan_set(ms->pat, ms->pat_len, p + 2u, &end, error)) {
                lusa_pat_set_error(ms, error);
                return false;
            }
            *out_end = end;
            return true;
        }
        *out_end = p + 2u;
        return true;
    }
    if (ms->pat[p] == '[') {
        char error[192];
        uint64_t end = 0u;
        if (!lusa_pat_scan_set(ms->pat, ms->pat_len, p, &end, error)) {
            lusa_pat_set_error(ms, error);
            return false;
        }
        *out_end = end;
        return true;
    }
    *out_end = p + 1u;
    return true;
}

static bool lusa_pat_bracket(const LusaMatcher *ms, uint32_t ch,
                             uint64_t p, uint64_t ep) {
    uint64_t i = p + 1u;
    bool negate = false;
    bool matched = false;
    if (i < ep && ms->pat[i] == '^') {
        negate = true;
        i += 1u;
    }
    if (i < ep && ms->pat[i] == ']') {
        if (ch == ']') matched = true;
        i += 1u;
    }
    while (i < ep) {
        if (ms->pat[i] == '%' && i + 1u < ep) {
            if (lusa_pat_class(ch, ms->pat[i + 1u])) matched = true;
            i += 2u;
        } else if (i + 2u < ep && ms->pat[i + 1u] == '-'
                   && ms->pat[i + 2u] != ']') {
            if (ms->pat[i] <= ch && ch <= ms->pat[i + 2u]) matched = true;
            i += 3u;
        } else {
            if (ms->pat[i] == ch) matched = true;
            i += 1u;
        }
    }
    return negate ? !matched : matched;
}

static bool lusa_pat_single(const LusaMatcher *ms, uint64_t s,
                            uint64_t p, uint64_t ep) {
    uint32_t ch;
    if (s >= ms->src_len) return false;
    ch = ms->src[s];
    if (ep > p && ms->pat[p] == '[') return lusa_pat_bracket(ms, ch, p, ep);
    if (ms->pat[p] == '.') return true;
    if (ms->pat[p] == '%' && p + 1u < ms->pat_len) {
        return lusa_pat_class(ch, ms->pat[p + 1u]);
    }
    return ms->pat[p] == ch;
}

static int64_t lusa_pat_do_match(LusaMatcher *ms, uint64_t s, uint64_t p,
                                 int32_t depth);
static int64_t lusa_pat_do_match_inner(LusaMatcher *ms, uint64_t s, uint64_t p,
                                       int32_t depth);

static int64_t lusa_pat_max_expand(LusaMatcher *ms, uint64_t s,
                                   uint64_t p, uint64_t ep, int32_t depth) {
    uint64_t count = 0u;
    while (lusa_pat_single(ms, s + count, p, ep)) count += 1u;
    for (;;) {
        int64_t result = lusa_pat_do_match(ms, s + count, ep + 1u, depth + 1);
        if (result != -1) return result;
        if (count == 0u) return -1;
        count -= 1u;
    }
}

static int64_t lusa_pat_min_expand(LusaMatcher *ms, uint64_t s,
                                   uint64_t p, uint64_t ep, int32_t depth) {
    for (;;) {
        int64_t result = lusa_pat_do_match(ms, s, ep + 1u, depth + 1);
        if (result != -1) return result;
        if (!lusa_pat_single(ms, s, p, ep)) return -1;
        s += 1u;
    }
}

static bool lusa_pat_start_capture(LusaMatcher *ms, uint64_t start,
                                   uint32_t kind) {
    if (ms->level >= (int32_t)LUSA_PAT_MAX_CAPTURES) {
        lusa_pat_set_error(ms, "malformed pattern: too many captures (maximum is 32)");
        return false;
    }
    ms->caps[ms->level].start = start;
    ms->caps[ms->level].end = start;
    ms->caps[ms->level].kind = kind;
    ms->level += 1;
    return true;
}

static bool lusa_pat_end_capture(LusaMatcher *ms, uint64_t end) {
    int32_t index = ms->level;
    while (index > 0) {
        index -= 1;
        if (ms->caps[index].kind == LUSA_PAT_OPEN) {
            ms->caps[index].end = end;
            ms->caps[index].kind = LUSA_PAT_SUBSTRING;
            return true;
        }
    }
    lusa_pat_set_error(ms, "malformed pattern: unmatched ')'");
    return false;
}

static int64_t lusa_pat_match_capture(LusaMatcher *ms, uint64_t s,
                                      uint32_t digit) {
    int32_t index = (int32_t)(digit - '0');
    uint64_t length;
    uint64_t i;
    if (index == 0 || index > ms->level) {
        snprintf(ms->error, sizeof(ms->error),
            "malformed pattern: invalid capture index %%%d", (int)index);
        ms->failed = true;
        return -2;
    }
    if (ms->caps[index - 1].kind != LUSA_PAT_SUBSTRING) {
        snprintf(ms->error, sizeof(ms->error),
            "malformed pattern: capture %%%d is not closed", (int)index);
        ms->failed = true;
        return -2;
    }
    length = ms->caps[index - 1].end - ms->caps[index - 1].start;
    if (s + length > ms->src_len) return -1;
    i = 0u;
    while (i < length) {
        if (ms->src[ms->caps[index - 1].start + i] != ms->src[s + i]) return -1;
        i += 1u;
    }
    return (int64_t)(s + length);
}

static int64_t lusa_pat_match_balance(LusaMatcher *ms, uint64_t s,
                                      uint64_t p) {
    uint32_t open;
    uint32_t close;
    int64_t depth;
    uint64_t i;
    if (p + 3u >= ms->pat_len) {
        lusa_pat_set_error(ms, "malformed pattern: '%b' needs two characters");
        return -2;
    }
    open = ms->pat[p + 2u];
    close = ms->pat[p + 3u];
    if (s >= ms->src_len || ms->src[s] != open) return -1;
    depth = 1;
    i = s + 1u;
    while (i < ms->src_len) {
        if (ms->src[i] == close) {
            depth -= 1;
            if (depth == 0) return (int64_t)(i + 1u);
        } else if (ms->src[i] == open) {
            depth += 1;
        }
        i += 1u;
    }
    return -1;
}

static int64_t lusa_pat_match_frontier(LusaMatcher *ms, uint64_t s,
                                       uint64_t p, uint64_t *next_p) {
    uint64_t ep;
    uint32_t previous;
    uint32_t current;
    if (p + 2u >= ms->pat_len || ms->pat[p + 2u] != '[') {
        lusa_pat_set_error(ms, "malformed pattern: '%f' must be followed by a set");
        return -2;
    }
    if (!lusa_pat_class_end(ms, p + 2u, &ep)) return -2;
    previous = s == 0u ? 0u : ms->src[s - 1u];
    current = s >= ms->src_len ? 0u : ms->src[s];
    if (lusa_pat_bracket(ms, previous, p + 2u, ep)
        || !lusa_pat_bracket(ms, current, p + 2u, ep)) {
        return -1;
    }
    *next_p = ep;
    return (int64_t)s;
}

static int64_t lusa_pat_do_match_inner(LusaMatcher *ms, uint64_t s, uint64_t p,
                                       int32_t depth) {
    if (depth > LUSA_PAT_MAX_DEPTH) {
        lusa_pat_set_error(ms,
            "pattern exceeded the bounded matching budget; simplify nested quantifiers");
        return -2;
    }
    if (!lusa_pat_tick(ms)) return -2;
    for (;;) {
        uint64_t ep;
        if (p >= ms->pat_len) return (int64_t)s;
        if (ms->pat[p] == '(') {
            if (p + 1u < ms->pat_len && ms->pat[p + 1u] == ')') {
                if (!lusa_pat_start_capture(ms, s, LUSA_PAT_POSITION)) return -2;
                p += 2u;
            } else {
                if (!lusa_pat_start_capture(ms, s, LUSA_PAT_OPEN)) return -2;
                p += 1u;
            }
            continue;
        }
        if (ms->pat[p] == ')') {
            if (!lusa_pat_end_capture(ms, s)) return -2;
            p += 1u;
            continue;
        }
        if (ms->pat[p] == '$' && p + 1u == ms->pat_len) {
            return s == ms->src_len ? (int64_t)s : -1;
        }
        if (ms->pat[p] == '%') {
            uint32_t esc;
            if (p + 1u >= ms->pat_len) {
                lusa_pat_set_error(ms, "malformed pattern: '%' at end of pattern");
                return -2;
            }
            esc = ms->pat[p + 1u];
            if (esc == 'b') {
                int64_t balanced = lusa_pat_match_balance(ms, s, p);
                if (balanced < 0) return balanced;
                s = (uint64_t)balanced;
                p += 4u;
                continue;
            }
            if (esc == 'f') {
                uint64_t next_p = 0u;
                int64_t frontier = lusa_pat_match_frontier(ms, s, p, &next_p);
                if (frontier < 0) return frontier;
                s = (uint64_t)frontier;
                p = next_p;
                continue;
            }
            if (esc >= '0' && esc <= '9') {
                int64_t captured = lusa_pat_match_capture(ms, s, esc);
                if (captured < 0) return captured;
                s = (uint64_t)captured;
                p += 2u;
                continue;
            }
        }
        if (!lusa_pat_class_end(ms, p, &ep)) return -2;
        if (ep < ms->pat_len) {
            if (ms->pat[ep] == '?') {
                uint64_t next = s;
                if (lusa_pat_single(ms, s, p, ep)) next += 1u;
                return lusa_pat_do_match(ms, next, ep + 1u, depth + 1);
            }
            if (ms->pat[ep] == '*') {
                return lusa_pat_max_expand(ms, s, p, ep, depth);
            }
            if (ms->pat[ep] == '+') {
                if (!lusa_pat_single(ms, s, p, ep)) return -1;
                return lusa_pat_max_expand(ms, s + 1u, p, ep, depth);
            }
            if (ms->pat[ep] == '-') {
                return lusa_pat_min_expand(ms, s, p, ep, depth);
            }
        }
        if (!lusa_pat_single(ms, s, p, ep)) return -1;
        s += 1u;
        p = ep;
    }
}

/* Captures are part of the matcher state and every recursive attempt must
 * observe the same entry state.  When a branch fails, restore the captures so
 * a sibling branch cannot see captures opened or closed by the failed one. */
static int64_t lusa_pat_do_match(LusaMatcher *ms, uint64_t s, uint64_t p,
                                 int32_t depth) {
    LusaPatCapture saved[LUSA_PAT_MAX_CAPTURES];
    int32_t saved_level = ms->level;
    int64_t result;
    memcpy(saved, ms->caps, sizeof(saved));
    result = lusa_pat_do_match_inner(ms, s, p, depth);
    if (result == -1) {
        memcpy(ms->caps, saved, sizeof(saved));
        ms->level = saved_level;
    }
    return result;
}

/* Returns 1 for a match, 0 for no match, and -1 for a runtime error. */
static int lusa_pat_search(const uint32_t *src, uint64_t src_len,
                           const uint32_t *pat, uint64_t pat_len,
                           uint64_t start, bool plain, bool honor_anchor,
                           LusaPatResult *result, char *error) {
    uint64_t s;
    uint64_t pstart = 0u;
    bool anchored = false;
    bool only_start = false;
    char validation[192];
    int32_t capture_count = 0;
    result->found = false;
    result->capture_count = 0;
    if (start > src_len) return 0;
    if (plain) {
        if (pat_len == 0u) {
            result->found = true;
            result->start = start;
            result->end = start;
            return 1;
        }
        if (pat_len > src_len - start) return 0;
        for (s = start; s + pat_len <= src_len; s += 1u) {
            uint64_t i = 0u;
            while (i < pat_len && src[s + i] == pat[i]) i += 1u;
            if (i == pat_len) {
                result->found = true;
                result->start = s;
                result->end = s + pat_len;
                return 1;
            }
        }
        return 0;
    }
    if (!lusa_pat_validate(pat, pat_len, &capture_count, validation)) {
        snprintf(error, 192, "%s", validation);
        return -1;
    }
    if (pat_len > 0u && pat[0] == '^') {
        anchored = true;
        pstart = 1u;
    }
    only_start = anchored && honor_anchor;
    s = start;
    for (;;) {
        LusaMatcher matcher;
        int64_t end;
        memset(&matcher, 0, sizeof(matcher));
        matcher.src = src;
        matcher.src_len = src_len;
        matcher.pat = pat;
        matcher.pat_len = pat_len;
        end = lusa_pat_do_match(&matcher, s, pstart, 0);
        if (end == -2) {
            snprintf(error, 192, "%s", matcher.error);
            return -1;
        }
        if (end >= 0) {
            int32_t i;
            result->found = true;
            result->start = s;
            result->end = (uint64_t)end;
            result->capture_count = matcher.level;
            i = 0;
            while (i < matcher.level) {
                result->captures[i] = matcher.caps[i];
                i += 1;
            }
            return 1;
        }
        if (s >= (only_start ? start : src_len)) return 0;
        s += 1u;
    }
}

static const char *lusa_pat_error_code(const char *error) {
    if (strncmp(error, "pattern exceeded ", 17u) == 0) return "E2011";
    return "E2010";
}

static bool lusa_pat_normalize_init(int32_t init, uint64_t length,
                                    uint64_t *start) {
    int64_t position = (int64_t)init;
    if (position < 0) position = (int64_t)length + position + 1;
    if (position < 1) position = 1;
    if ((uint64_t)position > length + 1u) return false;
    *start = (uint64_t)position - 1u;
    return true;
}

static LusaString *lusa_pat_capture_string(LusaString *source,
                                           const uint64_t *offsets,
                                           const LusaPatCapture *capture) {
    if (source == NULL) return lusa_string_new("", 0u);
    if (capture->kind == LUSA_PAT_POSITION) {
        char buffer[32];
        int written = snprintf(buffer, sizeof(buffer), "%llu",
            (unsigned long long)(capture->start + 1u));
        if (written < 0) written = 0;
        return lusa_string_new(buffer, (uint64_t)written);
    }
    return lusa_string_new(source->data + offsets[capture->start],
        offsets[capture->end] - offsets[capture->start]);
}

/* Decodes both strings, normalizes `init`, and searches once.  On success the
 * decoded source arrays are returned to the caller, which must free `src`,
 * `offsets`, and `pat`.  On failure all arrays are released here. */
static int lusa_pat_prepare_search(LusaString *source, LusaString *pattern,
                                   int32_t init, bool plain,
                                   LusaPatResult *out_result,
                                   uint32_t **out_src,
                                   uint64_t **out_offsets,
                                   uint64_t *out_src_len,
                                   char *error) {
    uint32_t *src = NULL;
    uint32_t *pat = NULL;
    uint64_t *offsets = NULL;
    uint64_t *pat_offsets = NULL;
    uint64_t src_len = 0u;
    uint64_t pat_len = 0u;
    uint64_t start = 0u;
    int status;
    lusa_pat_decode(source, &src, &offsets, &src_len);
    if (pattern == NULL) {
        pat = (uint32_t *)malloc(sizeof(uint32_t));
        if (pat == NULL) lusa_panic("out of memory");
        pat_len = 0u;
    } else {
        lusa_pat_decode(pattern, &pat, &pat_offsets, &pat_len);
        free(pat_offsets);
    }
    if (!lusa_pat_normalize_init(init, src_len, &start)) {
        free(src);
        free(pat);
        free(offsets);
        return 0;
    }
    status = lusa_pat_search(src, src_len, pat, pat_len, start, plain, true,
        out_result, error);
    if (status != 1) {
        free(src);
        free(pat);
        free(offsets);
        return status;
    }
    *out_src = src;
    *out_offsets = offsets;
    *out_src_len = src_len;
    free(pat);
    return 1;
}

int32_t lusa_string_find_start(LusaString *source, LusaString *pattern,
                               int32_t init, bool plain) {
    LusaPatResult result;
    uint32_t *src = NULL;
    uint64_t *offsets = NULL;
    uint64_t src_len = 0u;
    char error[192];
    int status = lusa_pat_prepare_search(source, pattern, init, plain,
        &result, &src, &offsets, &src_len, error);
    free(src);
    free(offsets);
    if (status < 0) {
        lusa_throw(lusa_pat_error_code(error), error);
        return 0;
    }
    return status == 0 ? 0 : (int32_t)(result.start + 1u);
}

bool lusa_string_find_values(LusaString *source, LusaString *pattern,
                             int32_t init, bool plain, int32_t *out_start,
                             int32_t *out_end) {
    LusaPatResult result;
    uint32_t *src = NULL;
    uint64_t *offsets = NULL;
    uint64_t src_len = 0u;
    char error[192];
    int status = lusa_pat_prepare_search(source, pattern, init, plain,
        &result, &src, &offsets, &src_len, error);
    free(src);
    free(offsets);
    if (status < 0) {
        lusa_throw(lusa_pat_error_code(error), error);
        if (out_start != NULL) *out_start = 0;
        if (out_end != NULL) *out_end = 0;
        return false;
    }
    if (status == 0) {
        if (out_start != NULL) *out_start = 0;
        if (out_end != NULL) *out_end = 0;
        return false;
    }
    if (out_start != NULL) *out_start = (int32_t)(result.start + 1u);
    if (out_end != NULL) *out_end = (int32_t)result.end;
    return true;
}

LusaString *lusa_string_match(LusaString *source, LusaString *pattern,
                              int32_t init) {
    LusaPatResult result;
    uint32_t *src = NULL;
    uint64_t *offsets = NULL;
    uint64_t src_len = 0u;
    char error[192];
    LusaString *text = NULL;
    int status = lusa_pat_prepare_search(source, pattern, init, false,
        &result, &src, &offsets, &src_len, error);
    if (status < 0) {
        free(src);
        free(offsets);
        lusa_throw(lusa_pat_error_code(error), error);
        return NULL;
    }
    if (status == 0) {
        free(src);
        free(offsets);
        return NULL;
    }
    if (result.capture_count > 0) {
        text = lusa_pat_capture_string(source, offsets, &result.captures[0]);
    } else if (source == NULL) {
        text = lusa_string_new("", 0u);
    } else {
        text = lusa_string_new(source->data + offsets[result.start],
            offsets[result.end] - offsets[result.start]);
    }
    free(src);
    free(offsets);
    return text;
}

/* ===== gsub ===== */

typedef struct LusaPatBuffer {
    char *data;
    uint64_t length;
    uint64_t capacity;
} LusaPatBuffer;

static void lusa_pat_buffer_init(LusaPatBuffer *buffer) {
    buffer->data = NULL;
    buffer->length = 0u;
    buffer->capacity = 0u;
}

static void lusa_pat_buffer_reserve(LusaPatBuffer *buffer, uint64_t extra) {
    uint64_t needed = buffer->length + extra;
    uint64_t capacity = buffer->capacity;
    char *grown;
    if (needed <= capacity) return;
    if (capacity == 0u) capacity = 32u;
    while (capacity < needed) capacity *= 2u;
    grown = (char *)realloc(buffer->data, (size_t)capacity);
    if (grown == NULL) lusa_panic("out of memory");
    buffer->data = grown;
    buffer->capacity = capacity;
}

static void lusa_pat_buffer_append(LusaPatBuffer *buffer, const char *data,
                                   uint64_t length) {
    if (length == 0u) return;
    lusa_pat_buffer_reserve(buffer, length);
    memcpy(buffer->data + buffer->length, data, (size_t)length);
    buffer->length += length;
}

static void lusa_pat_buffer_byte(LusaPatBuffer *buffer, char value) {
    lusa_pat_buffer_reserve(buffer, 1u);
    buffer->data[buffer->length] = value;
    buffer->length += 1u;
}

static void lusa_pat_buffer_source(LusaPatBuffer *buffer, LusaString *source,
                                   const uint64_t *offsets,
                                   uint64_t start, uint64_t end) {
    if (source == NULL || end <= start) return;
    lusa_pat_buffer_append(buffer, source->data + offsets[start],
        offsets[end] - offsets[start]);
}

static bool lusa_pat_expand_replacement(LusaPatBuffer *buffer,
                                        LusaString *source,
                                        const uint64_t *offsets,
                                        LusaString *replacement,
                                        const LusaPatResult *match,
                                        char *error) {
    uint64_t i = 0u;
    if (replacement == NULL) return true;
    while (i < replacement->obj.length) {
        unsigned char ch = (unsigned char)replacement->data[i];
        if (ch != '%') {
            lusa_pat_buffer_byte(buffer, (char)ch);
            i += 1u;
            continue;
        }
        i += 1u;
        if (i >= replacement->obj.length) {
            snprintf(error, 192, "%s", "invalid use of '%' in replacement string");
            return false;
        }
        ch = (unsigned char)replacement->data[i];
        i += 1u;
        if (ch == '%') {
            lusa_pat_buffer_byte(buffer, '%');
        } else if (ch >= '0' && ch <= '9') {
            int32_t index = (int32_t)(ch - '0');
            if (index > match->capture_count) {
                snprintf(error, 192,
                    "replacement references capture %%%d but the pattern has %d capture(s)",
                    (int)index, (int)match->capture_count);
                return false;
            }
            if (index == 0) {
                lusa_pat_buffer_source(buffer, source, offsets,
                    match->start, match->end);
            } else if (match->captures[index - 1].kind == LUSA_PAT_POSITION) {
                char number[32];
                int written = snprintf(number, sizeof(number), "%llu",
                    (unsigned long long)(match->captures[index - 1].start + 1u));
                if (written < 0) written = 0;
                lusa_pat_buffer_append(buffer, number, (uint64_t)written);
            } else {
                lusa_pat_buffer_source(buffer, source, offsets,
                    match->captures[index - 1].start,
                    match->captures[index - 1].end);
            }
        } else {
            snprintf(error, 192,
                "invalid replacement escape '%%%c'; use '%%0'..'%%9' or '%%%%'",
                (int)ch);
            return false;
        }
    }
    return true;
}

LusaString *lusa_string_gsub(LusaString *source, LusaString *pattern,
                             LusaString *replacement, int32_t limit,
                             int32_t *out_count) {
    uint32_t *src = NULL;
    uint32_t *pat = NULL;
    uint64_t *offsets = NULL;
    uint64_t src_len = 0u;
    uint64_t pat_len = 0u;
    int32_t capture_count = 0;
    char error[192];
    LusaPatBuffer buffer;
    uint64_t s = 0u;
    int32_t count = 0;
    int32_t max = limit < 0 ? -1 : limit;
    bool anchored = false;
    LusaString *out = NULL;

    lusa_pat_decode(source, &src, &offsets, &src_len);
    if (pattern == NULL) {
        pat = (uint32_t *)malloc(sizeof(uint32_t));
        if (pat == NULL) lusa_panic("out of memory");
        pat_len = 0u;
    } else {
        uint64_t *unused = NULL;
        lusa_pat_decode(pattern, &pat, &unused, &pat_len);
        free(unused);
    }
    if (!lusa_pat_validate(pat, pat_len, &capture_count, error)) {
        free(src);
        free(pat);
        free(offsets);
        lusa_throw(lusa_pat_error_code(error), error);
        if (out_count != NULL) *out_count = 0;
        return NULL;
    }
    if (pat_len > 0u && pat[0] == '^') anchored = true;
    lusa_pat_buffer_init(&buffer);
    if (max == 0) {
        lusa_pat_buffer_source(&buffer, source, offsets, 0u, src_len);
    } else if (anchored) {
        LusaPatResult result;
        char search_error[192];
        int status = lusa_pat_search(src, src_len, pat, pat_len, 0u, false,
            true, &result, search_error);
        if (status < 0) {
            snprintf(error, 192, "%s", search_error);
            goto gsub_error;
        }
        if (status == 0) {
            lusa_pat_buffer_source(&buffer, source, offsets, 0u, src_len);
        } else {
            lusa_pat_buffer_source(&buffer, source, offsets, 0u, result.start);
            if (!lusa_pat_expand_replacement(&buffer, source, offsets,
                    replacement, &result, error)) {
                goto gsub_error;
            }
            lusa_pat_buffer_source(&buffer, source, offsets,
                result.end, src_len);
            count = 1;
        }
    } else {
        for (;;) {
            LusaPatResult result;
            char search_error[192];
            int status;
            if (max >= 0 && count >= max) {
                lusa_pat_buffer_source(&buffer, source, offsets, s, src_len);
                break;
            }
            status = lusa_pat_search(src, src_len, pat, pat_len, s, false,
                true, &result, search_error);
            if (status < 0) {
                snprintf(error, 192, "%s", search_error);
                goto gsub_error;
            }
            if (status == 0) {
                lusa_pat_buffer_source(&buffer, source, offsets, s, src_len);
                break;
            }
            lusa_pat_buffer_source(&buffer, source, offsets, s, result.start);
            if (!lusa_pat_expand_replacement(&buffer, source, offsets,
                    replacement, &result, error)) {
                goto gsub_error;
            }
            count += 1;
            if (result.end > result.start) {
                s = result.end;
            } else if (result.end < src_len) {
                lusa_pat_buffer_source(&buffer, source, offsets,
                    result.end, result.end + 1u);
                s = result.end + 1u;
            } else {
                break;
            }
        }
    }
    out = lusa_string_new(buffer.data == NULL ? "" : buffer.data, buffer.length);
    free(buffer.data);
    free(src);
    free(pat);
    free(offsets);
    if (out_count != NULL) *out_count = count;
    return out;

gsub_error:
    free(buffer.data);
    free(src);
    free(pat);
    free(offsets);
    lusa_throw(lusa_pat_error_code(error), error);
    if (out_count != NULL) *out_count = 0;
    return NULL;
}

/* ===== gmatch ===== */

LusaGmatch *lusa_string_gmatch_new(LusaString *source, LusaString *pattern) {
    LusaGmatch *iterator = (LusaGmatch *)calloc(1, sizeof(LusaGmatch));
    char error[192];
    int32_t captures = 0;
    if (iterator == NULL) {
        lusa_panic("out of memory");
        return NULL;
    }
    iterator->source = source == NULL ? lusa_string_new("", 0u)
                                      : lusa_string_clone(source);
    lusa_pat_decode(iterator->source, &iterator->src, &iterator->offsets,
        &iterator->src_len);
    if (pattern == NULL) {
        iterator->pat = (uint32_t *)malloc(sizeof(uint32_t));
        if (iterator->pat == NULL) lusa_panic("out of memory");
        iterator->pat_len = 0u;
    } else {
        uint64_t *unused = NULL;
        lusa_pat_decode(pattern, &iterator->pat, &unused, &iterator->pat_len);
        free(unused);
    }
    if (!lusa_pat_validate(iterator->pat, iterator->pat_len, &captures, error)) {
        lusa_drop_gmatch(&iterator);
        lusa_throw(lusa_pat_error_code(error), error);
        return NULL;
    }
    iterator->pos = 0u;
    iterator->done = false;
    return iterator;
}

bool lusa_string_gmatch_next(LusaGmatch *iterator, LusaString **out,
                             int32_t count) {
    LusaPatResult result;
    char error[192];
    int status;
    int32_t i;
    if (iterator == NULL || iterator->done || iterator->pos > iterator->src_len) {
        return false;
    }
    status = lusa_pat_search(iterator->src, iterator->src_len,
        iterator->pat, iterator->pat_len, iterator->pos, false, false,
        &result, error);
    if (status < 0) {
        lusa_throw(lusa_pat_error_code(error), error);
        return false;
    }
    if (status == 0) {
        iterator->done = true;
        return false;
    }
    for (i = 0; i < count; i++) out[i] = NULL;
    if (result.capture_count > 0) {
        for (i = 0; i < count && i < result.capture_count; i++) {
            out[i] = lusa_pat_capture_string(iterator->source, iterator->offsets,
                &result.captures[i]);
        }
    } else if (count > 0) {
        out[0] = lusa_string_new(iterator->source->data + iterator->offsets[result.start],
            iterator->offsets[result.end] - iterator->offsets[result.start]);
    }
    if (result.end > result.start) {
        iterator->pos = result.end;
    } else if (result.end < iterator->src_len) {
        iterator->pos = result.end + 1u;
    } else {
        iterator->done = true;
    }
    return true;
}

static void lusa_drop_gmatch(LusaGmatch **value) {
    LusaGmatch *iterator;
    if (value == NULL) return;
    iterator = *value;
    if (iterator == NULL) return;
    free(iterator->src);
    free(iterator->offsets);
    free(iterator->pat);
    if (iterator->source != NULL) lusa_drop_string(&iterator->source);
    free(iterator);
    *value = NULL;
}
