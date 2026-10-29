"""Compatibility imports for the merged bug-bounty-cli distribution."""
from bb_cli import __version__
import importlib
import sys
_ALIASES = {'cli': 'bb_cli.providers.cantina.cli', 'client': 'bb_cli.providers.cantina.client', 'render': 'bb_cli.providers.cantina.render', 'cache': 'bb_cli.cache', 'rsc': 'bb_cli.rsc', 'contracts': 'bb_cli.providers.cantina.contracts', 'discovery': 'bb_cli.providers.cantina.discovery', '_evm': 'bb_cli.evm'}
for _name, _target in _ALIASES.items():
    _module = importlib.import_module(_target)
    globals()[_name] = _module
    sys.modules[__name__ + "." + _name] = _module

for _name in ('abi','cache','client','contract_cli','explorers','keccak','onchain','render','rpc'):
    _target = 'bb_cli.cache' if _name == 'cache' else 'bb_cli.evm.' + _name
    sys.modules[__name__ + '._evm.' + _name] = importlib.import_module(_target)
