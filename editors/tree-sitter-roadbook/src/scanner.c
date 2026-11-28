// The one token the grammar cannot lex by itself: an `after` on the same line as what comes
// before it. A delayed raise (`raise Ping after 5s`) goes on with an `after` only on the line
// its event, or its `}`, ends on; on a later line, `after` starts an `after` transition, as
// in the compiler. Blanks and tabs before it are skipped, a newline is not.

#include "tree_sitter/parser.h"

enum TokenType {
  SAME_LINE_AFTER,
  // Never valid in the grammar: valid only during error recovery, when the scanner does
  // nothing, so that it does not turn the `after` of a transition into this token.
  ERROR_SENTINEL,
};

void *tree_sitter_roadbook_external_scanner_create(void) { return NULL; }

void tree_sitter_roadbook_external_scanner_destroy(void *payload) { (void)payload; }

unsigned tree_sitter_roadbook_external_scanner_serialize(void *payload, char *buffer) {
  (void)payload;
  (void)buffer;
  return 0;
}

void tree_sitter_roadbook_external_scanner_deserialize(void *payload, const char *buffer,
                                                       unsigned length) {
  (void)payload;
  (void)buffer;
  (void)length;
}

static bool is_name_char(int32_t c) {
  return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9');
}

bool tree_sitter_roadbook_external_scanner_scan(void *payload, TSLexer *lexer,
                                                const bool *valid_symbols) {
  (void)payload;
  if (valid_symbols[ERROR_SENTINEL] || !valid_symbols[SAME_LINE_AFTER]) return false;
  while (lexer->lookahead == ' ' || lexer->lookahead == '\t') lexer->advance(lexer, true);
  for (const char *c = "after"; *c; c++) {
    if (lexer->lookahead != *c) return false;
    lexer->advance(lexer, false);
  }
  if (is_name_char(lexer->lookahead)) return false;
  lexer->result_symbol = SAME_LINE_AFTER;
  return true;
}
