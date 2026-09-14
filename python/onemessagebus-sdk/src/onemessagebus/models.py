"""The generated model of every message the registry holds, by its family's name.

`PlannerSurface` is `agent.planner-surface@1`'s model at the latest version the
registry holds, and `PlannerSurfaceV1` names that version; `messages.MESSAGES`
maps each id to its model. Send one, and read one back with `next(type=...)`.
"""

from ._generated.models import *  # noqa: F403 - the generated module's __all__ is this module's
from ._generated.models import __all__ as __all__
