# SSH keys for deployment

No keys are shipped in this repository, by design.

Generate your own before deploying:

```bash
ssh-keygen -t ed25519 -a 100 -C "khaen-deploy-$(date +%Y)" -f ./deploy/ssh/deploy_ed25519
cat ./deploy/ssh/deploy_ed25519.pub   # install this on the target's authorized_keys
```

`.gitignore` already excludes `deploy/ssh/deploy_ed25519` and `deploy/ssh/id_*`.
Never commit a private key, and never commit a site-specific `authorized_keys`.
