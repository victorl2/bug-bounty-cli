"""Compatibility imports for the merged bug-bounty-cli distribution."""
from bb_cli import __version__
import importlib
import sys
_ALIASES = {'cli': 'bb_cli.providers.immunefi.cli', 'client': 'bb_cli.providers.immunefi.client', 'render': 'bb_cli.providers.immunefi.render', 'cache': 'bb_cli.cache', 'rsc': 'bb_cli.rsc', 'abi': 'bb_cli.evm.abi', 'keccak': 'bb_cli.evm.keccak', 'contract_cli': 'bb_cli.evm.contract_cli', 'explorers': 'bb_cli.evm.explorers', 'onchain': 'bb_cli.evm.onchain', 'rpc': 'bb_cli.evm.rpc'}
for _name, _target in _ALIASES.items():
    _module = importlib.import_module(_target)
    globals()[_name] = _module
    sys.modules[__name__ + "." + _name] = _module
