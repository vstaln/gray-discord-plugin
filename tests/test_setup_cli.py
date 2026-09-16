"""Successful pairing flows into registration and optional service startup."""
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import AsyncMock, patch
from gray_discord.cli import main


class SetupCliTests(unittest.TestCase):
    def test_completed_setup_registers_and_optionally_starts(self):
        for answer, start in [('y', True), ('', True), ('n', False)]:
            with self.subTest(answer=answer), tempfile.TemporaryDirectory() as tmp:
                path = Path(tmp) / 'config.json'
                config = dict(gray_home=tmp)
                with patch.object(sys, 'argv', ['gray-discord', '--config', str(path), 'setup']), \
                     patch('gray_discord.setup.setup', new=AsyncMock(return_value=True)), \
                     patch('gray_discord.cli.load_config', return_value=config), \
                     patch('gray_discord.cli.register') as register, \
                     patch('gray_discord.cli.service.install') as install, \
                     patch('builtins.input', return_value=answer):
                    main()
                    register.assert_called_once_with(config, path)
                    self.assertEqual(install.called, start)

    def test_cancelled_setup_does_not_register_or_start(self):
        with patch.object(sys, 'argv', ['gray-discord', 'setup']), \
             patch('gray_discord.setup.setup', new=AsyncMock(return_value=False)), \
             patch('gray_discord.cli.register') as register, \
             patch('gray_discord.cli.service.install') as install:
            main()
            register.assert_not_called()
            install.assert_not_called()
