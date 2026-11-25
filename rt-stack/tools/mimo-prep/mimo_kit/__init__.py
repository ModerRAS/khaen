"""mimo_kit -- fixed KHAEN MIMO planrt analysis kit (task-249, 2026-11-19).

Modules:
  s24.py              byte-exact S24_3LE helpers (stdlib)
  extract.py          fixed board-side arm extractor (replaces planrt_extract.py)
  analyze.py          fixed PC-side before/after analyzer (replaces planrt_analyze.py)
  provenance.py       task-252 fail-closed guard: reject synthetic dry-run G / invalid H,
                      and refuse the LIVE t66 "no design source -> prep/dryrun/out/design" fallback
  reference_design.py maintained copy of the pilot_3mic G objective (bit-parity with the
                      legacy dry-run design_g; carries real-H provenance, no dry-run import)
  design_chain.py     guarded LIVE chain: measured arm -> H guard -> real-room G -> G guard
                      -> T66 export -> REAL T66 compiler -> plan guard -> provenance manifest

Deploy to the board as planrt_extract.py + s24.py (same directory).
"""
__version__ = "1.0.0"
KIT_VERSION = "mimo_kit/1.0.0"
