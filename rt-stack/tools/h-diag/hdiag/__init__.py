"""h-diag: complex transfer-matrix diagnostics for the 4.1.2 low-band MIMO matrix.

Read-only diagnostic tool: it never modifies crates/modal-design and never touches
the board.  See README.md for the input manifest schema, CLI and metric definitions.
"""
from .guards import TOOL_NAME, TOOL_VERSION  # noqa: F401
