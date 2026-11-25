import os
import sys
import tempfile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
sys.modules["numpy"] = None  # 阻断 numpy，验证 plan/run --dry-run 纯标准库
import khaen_cal_batch as cli

tmp = tempfile.mkdtemp(prefix="calib-batch-lazy-")
rc1 = cli.main(["plan", "--config", "configs/batch-config.production-4.1.2.json",
                "--round", "A", "--out", os.path.join(tmp, "plan")])
print("PLAN_RC", rc1)
rc2 = cli.main(["run", "--plan", os.path.join(tmp, "plan", "plan.json"),
                "--transport", "cmd", "--dry-run", "--work", os.path.join(tmp, "work")])
print("DRYRUN_RC", rc2)
rc3 = cli.main(["version"])
print("VERSION_RC", rc3)
sys.exit(0 if (rc1 == 0 and rc2 == 0 and rc3 == 0) else 1)
