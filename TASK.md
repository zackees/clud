# OBJECTIVE: LAZY LOAD telegram and other things until it's actually called

### BACKGROUND:

Most of the web ui stuff isn't used most of the time. Therefore loading it unconditionally is unwarrented. Investigate the log below made during ctrl-c and figure out how to lazy load these "extras"

### LOG

Traceback (most recent call last):
  File "<frozen runpy>", line 198, in _run_module_as_main
  File "<frozen runpy>", line 88, in _run_code
  File "C:\tools\python13\Scripts\clud.exe\__main__.py", line 2, in <module>
  File "C:\tools\python13\Lib\site-packages\clud\cli.py", line 7, in <module>
    from .agent_cli import main as agent_main
  File "C:\tools\python13\Lib\site-packages\clud\agent_cli.py", line 6, in <module>
    from .agent.api_key import handle_login
  File "C:\tools\python13\Lib\site-packages\clud\agent\__init__.py", line 24, in <module>
    from clud.agent.hooks import register_hooks_from_config, trigger_hook_sync
  File "C:\tools\python13\Lib\site-packages\clud\agent\hooks.py", line 12, in <module>
    from clud.hooks.telegram import TelegramHookHandler
  File "C:\tools\python13\Lib\site-packages\clud\hooks\telegram.py", line 14, in <module>
    from clud.telegram.api_interface import TelegramBotAPI
  File "C:\tools\python13\Lib\site-packages\clud\telegram\__init__.py", line 7, in <module>
    from clud.telegram.api import create_telegram_api_router
  File "C:\tools\python13\Lib\site-packages\clud\telegram\api.py", line 10, in <module>
    from fastapi import APIRouter, HTTPException, status
  File "C:\tools\python13\Lib\site-packages\fastapi\__init__.py", line 7, in <module>
    from .applications import FastAPI as FastAPI
  File "C:\tools\python13\Lib\site-packages\fastapi\applications.py", line 17, in <module>
    from fastapi import routing
  File "C:\tools\python13\Lib\site-packages\fastapi\routing.py", line 28, in <module>
    from fastapi import params, temp_pydantic_v1_params
  File "C:\tools\python13\Lib\site-packages\fastapi\params.py", line 6, in <module>
    from fastapi.openapi.models import Example
  File "C:\tools\python13\Lib\site-packages\fastapi\openapi\models.py", line 4, in <module>
    from fastapi._compat import (
  File "C:\tools\python13\Lib\site-packages\fastapi\_compat\__init__.py", line 1, in <module>
    from .main import BaseConfig as BaseConfig
  File "C:\tools\python13\Lib\site-packages\fastapi\_compat\main.py", line 12, in <module>
    from fastapi._compat import may_v1
  File "C:\tools\python13\Lib\site-packages\fastapi\_compat\may_v1.py", line 4, in <module>
    from fastapi.types import ModelNameMap
  File "C:\tools\python13\Lib\site-packages\fastapi\types.py", line 5, in <module>
    from pydantic import BaseModel
  File "C:\tools\python13\Lib\site-packages\pydantic\__init__.py", line 413, in <module>
    _getattr_migration = getattr_migration(__name__)
  File "C:\tools\python13\Lib\site-packages\pydantic\_migration.py", line 260, in getattr_migration
    from .errors import PydanticImportError
  File "C:\tools\python13\Lib\site-packages\pydantic\errors.py", line 9, in <module>
    from typing_inspection.introspection import Qualifier
  File "C:\tools\python13\Lib\site-packages\typing_inspection\introspection.py", line 14, in <module>
    from . import typing_objects
  File "C:\tools\python13\Lib\site-packages\typing_inspection\typing_objects.py", line 415, in <module>
    is_readonly = _compile_identity_check_function('ReadOnly', 'is_readonly')
  File "C:\tools\python13\Lib\site-packages\typing_inspection\typing_objects.py", line 100, in _compile_identity_check_function
    exec(func_code, globals_, locals_)
  File "<string>", line 0, in <module>
KeyboardInterrupt