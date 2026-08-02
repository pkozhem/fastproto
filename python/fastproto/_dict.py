"""Native (non-JSON) dict conversion for fastproto messages.

``to_dict`` recursively unfolds a message into a plain Python dict — nested
messages become dicts, ``repeated``/``map`` become list/dict, and everything
else (``IntEnum`` members, ``datetime``/``timedelta``, ``bytes``, scalars) is
kept as-is. ``from_dict`` rebuilds the message from such a dict. These dicts are
for in-process use, not a wire/JSON format, so no base64/RFC3339/int64-string
conversion happens. The pair guarantees ``Cls.from_dict(x.to_dict()) == x``.
"""

import types
from dataclasses import is_dataclass
from enum import IntEnum
from functools import cache
from typing import TYPE_CHECKING, Union, cast, get_args, get_origin, get_type_hints

from ._core import Descriptor

if TYPE_CHECKING:
    from collections.abc import Callable, Mapping

    from . import Message

# ``get_origin`` of ``X | None`` is ``types.UnionType`` for a PEP 604 union but
# ``typing.Union`` when the hint came from a string forward-ref; accept both.
_UNION_ORIGINS = (Union, types.UnionType)

__all__ = ("from_dict", "to_dict")


def to_dict(msg: "Message") -> dict[str, object]:
    """Recursively convert a message into a native Python dict."""
    fields = cast("dict[str, object]", getattr(msg, "__dataclass_fields__", {}))
    return {name: _to_value(getattr(msg, name)) for name in fields}


def _to_value(value: object) -> object:
    """Convert one field value to its native-dict form, recursing into messages."""
    if value is None:
        return None
    if is_dataclass(value) and not isinstance(value, type):  # nested message instance
        return to_dict(cast("Message", value))
    if isinstance(value, list):  # repeated
        return [_to_value(item) for item in cast("list[object]", value)]
    if isinstance(value, dict):  # map: recurse values, keep scalar keys
        pairs = cast("dict[object, object]", value).items()
        return {key: _to_value(item) for key, item in pairs}
    return value  # IntEnum member, datetime, timedelta, bytes, scalar -> as-is


def from_dict[MessageT: "Message"](
    cls: "type[MessageT]",
    data: "Mapping[str, object]",
) -> "MessageT":
    """Rebuild a message from a native dict.

    A missing key falls back to the field's default; an unknown key raises
    :class:`ValueError`. Guarantees ``cls.from_dict(x.to_dict()) == x``.
    """
    hints, valid = _spec(cls)
    kwargs: dict[str, object] = {}
    for key, value in data.items():
        if key not in valid:
            names = sorted(valid)
            msg = f"{cls.__name__!r} has no field {key!r}; valid fields: {names}"
            raise ValueError(msg)
        kwargs[key] = _from_value(hints[key], value)
    return cast("Callable[..., MessageT]", cls)(**kwargs)


def _from_value(hint: object, value: object) -> object:
    """Convert a dict value back to the runtime type described by ``hint``."""
    if value is None:
        return None
    tp = hint
    if hasattr(tp, "__metadata__"):  # Annotated[T, _ScalarMeta] -> T
        tp = get_args(tp)[0]
    origin = get_origin(tp)
    if origin in _UNION_ORIGINS:  # X | None -> recurse into the non-None arm
        tp = next(arg for arg in get_args(tp) if arg is not type(None))
        return _from_value(tp, value)
    if origin is list and isinstance(value, list):
        (elem,) = get_args(tp)
        return [_from_value(elem, item) for item in cast("list[object]", value)]
    if origin is dict and isinstance(value, dict):
        val_hint = get_args(tp)[1]  # keys stay as-is; values recurse
        pairs = cast("dict[object, object]", value).items()
        return {key: _from_value(val_hint, item) for key, item in pairs}
    return _from_leaf(tp, value)


def _from_leaf(tp: object, value: object) -> object:
    """Rebuild a scalar / enum / nested-message leaf from its resolved type."""
    if isinstance(tp, type):
        if is_dataclass(tp) and isinstance(value, dict):  # nested message
            return from_dict(
                cast("type[Message]", tp),
                cast("Mapping[str, object]", value),
            )
        if issubclass(tp, IntEnum):  # coerce; an open-enum value stays a raw int
            try:
                return tp(value)
            except ValueError:
                return value
    return value  # datetime, timedelta, bytes, scalar -> as-is


@cache
def _spec(cls: "type[Message]") -> "tuple[dict[str, object], frozenset[str]]":
    """Resolve ``cls``'s field type hints and valid field-name set (cached).

    ``get_type_hints`` walks the MRO and evaluates the base's
    ``__fastproto__: ClassVar[Descriptor]`` annotation, so ``Descriptor`` must be
    in scope — it lives only under ``TYPE_CHECKING`` in ``__init__``, hence the
    explicit ``localns``.
    """
    hints = get_type_hints(cls, include_extras=True, localns={"Descriptor": Descriptor})
    return hints, frozenset(cls.__fastproto__.field_names())
