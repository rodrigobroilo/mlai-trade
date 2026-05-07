# Local Configuration

This directory is for local runtime configuration only. Do not commit API keys, secrets, tokens, account numbers, private certificates, or generated config containing credentials.

Use untracked local files for secrets. Example files may be committed only when they contain placeholders.

`mlai-trade.json` is the private runtime config and must not be committed.

`mlai-trade.example.json` is the authoritative public schema example. Keep every supported config key explicit there, even when the code has a default. Runtime configs should be updated from this example without overwriting local credentials.

`mlai-trade-ml-tuning.json` is the private ML hyperparameter tuning config and must not be committed.

`mlai-trade-ml-tuning.example.json` is the public LSTM tuning example. It keeps research knobs such as CPU/MLX/TCH profile selection, target mode, hidden width, epochs, learning rate, loss function, dropout, weight decay, and early stopping outside the provider/API/daemon config.

`tax-brackets.json` contains public IRS bracket/rate data used by `mlai-trade compliance tax`.
Start from `tax-brackets.example.json`, then update it by JSON diff when IRS publishes a new tax year.
