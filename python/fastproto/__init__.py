"""FastProto: readable, Pythonic protobuf messages backed by a Rust core.

Generated modules define plain ``@dataclass`` message types annotated with
``Scalar.*`` field types and decorated with :func:`message`, which wires the
class to the native (de)serialization engine.
"""

import sys
from dataclasses import MISSING
from typing import TYPE_CHECKING, Annotated, ClassVar, Self, cast, override

from ._core import compile_descriptor

if TYPE_CHECKING:
    from collections.abc import Callable

    from ._core import Descriptor

__all__ = ["Message", "Scalar", "message"]


class Message:
    """Base class for generated message dataclasses.

    Provides the serialization API; the :func:`message` decorator attaches the
    compiled descriptor as ``__fastproto__``. Generated classes are plain
    ``@dataclass`` types that inherit from this, so ``to_bytes`` / ``from_bytes``
    are fully typed for callers.

    The ``_fastproto_unknown`` slot holds the raw wire bytes of fields the
    schema doesn't know about: the decoder stores them and the encoder re-emits
    them, so decode -> encode round-trips preserve fields added by newer
    producers (protobuf forward compatibility). It is deliberately *not* a
    dataclass field — adding an annotation here would make ``@dataclass`` treat
    it as an ``__init__`` parameter on every generated class.
    """

    __slots__ = ("_fastproto_unknown",)
    __fastproto__: "ClassVar[Descriptor]"

    def __post_init__(self) -> None:
        """Initialize the unknown-fields slot.

        Generated ``@dataclass`` ``__init__`` methods call this automatically.
        An initialized slot keeps the encoder's per-call read of it a plain
        attribute hit; on instances without it (hand-rolled subclasses that
        never run ``__init__``) the encoder falls back to catching
        ``AttributeError``, which is correct but measurably slower.
        """
        self._fastproto_unknown = b""

    def to_bytes(self) -> bytes:
        """Serialize this message to protobuf wire bytes."""
        # The linked check is inlined (rather than always calling
        # _ensure_linked) to keep the steady-state path to one native call.
        descriptor = self.__fastproto__
        if not descriptor.is_linked:
            _ensure_linked(type(self))
        return descriptor.encode(self)

    @classmethod
    def from_bytes(cls, data: bytes) -> Self:
        """Deserialize protobuf wire bytes into a new instance of ``cls``."""
        descriptor = cls.__fastproto__
        if not descriptor.is_linked:
            _ensure_linked(cls)
        return descriptor.decode(cls, data)

    def which_oneof(self, name: str) -> str | None:
        """Return the set member of oneof group ``name``, or ``None`` if unset.

        Mirrors google-protobuf's ``WhichOneof``: members are plain ``T | None``
        fields, and this reports which one currently holds a value. Raises
        :class:`ValueError` if ``name`` is not a oneof group of this message.
        """
        for group, members in self.__fastproto__.oneofs():
            if group == name:
                return next((m for m in members if getattr(self, m) is not None), None)
        available = [group for group, _ in self.__fastproto__.oneofs()]
        msg = f"{type(self).__name__!r} has no oneof group {name!r}; got {available}"
        raise ValueError(msg)


class _ScalarMeta:
    """Marker attached to a ``Scalar.*`` annotation recording the proto type.

    Purely informational: the actual wire type comes from the compiled
    descriptor, so this never affects encoding. It exists so tools and humans
    can recover the precise proto type from an annotation.
    """

    __slots__ = ("proto_name",)

    proto_name: str

    def __init__(self, proto_name: str) -> None:
        self.proto_name = proto_name

    @override
    def __repr__(self) -> str:
        return f"proto:{self.proto_name}"


class Scalar:
    """Namespace of proto scalar field types.

    Each alias is an ``Annotated`` wrapper over the underlying Python type, so
    ``Scalar.Int64`` type-checks exactly as ``int`` while still declaring, for
    the reader, that the field is a proto ``int64``.
    """

    Double = Annotated[float, _ScalarMeta("double")]
    Float = Annotated[float, _ScalarMeta("float")]
    Int32 = Annotated[int, _ScalarMeta("int32")]
    Int64 = Annotated[int, _ScalarMeta("int64")]
    UInt32 = Annotated[int, _ScalarMeta("uint32")]
    UInt64 = Annotated[int, _ScalarMeta("uint64")]
    SInt32 = Annotated[int, _ScalarMeta("sint32")]
    SInt64 = Annotated[int, _ScalarMeta("sint64")]
    Fixed32 = Annotated[int, _ScalarMeta("fixed32")]
    Fixed64 = Annotated[int, _ScalarMeta("fixed64")]
    SFixed32 = Annotated[int, _ScalarMeta("sfixed32")]
    SFixed64 = Annotated[int, _ScalarMeta("sfixed64")]
    Bool = Annotated[bool, _ScalarMeta("bool")]
    String = Annotated[str, _ScalarMeta("string")]
    Bytes = Annotated[bytes, _ScalarMeta("bytes")]


def message[MessageT: Message](
    descriptor: bytes,
) -> "Callable[[type[MessageT]], type[MessageT]]":
    """Bind a :class:`Message` dataclass to its compiled descriptor.

    Parses the embedded ``DescriptorProto`` bytes once and stores the result on
    the class as ``__fastproto__`` for :meth:`Message.to_bytes` /
    :meth:`Message.from_bytes` to use.
    """
    compiled = compile_descriptor(descriptor)

    def bind(cls: type[MessageT]) -> type[MessageT]:
        cls.__fastproto__ = compiled
        return cls

    return bind


def _resolve(namespace: dict[str, object], qualified: str) -> type:
    """Look up a (possibly nested) type by its qualified proto name.

    ``qualified`` is a full proto path minus the leading dot, e.g.
    ``pkg.Outer.Inner``. The package prefix is not reflected in the module
    namespace, and we can't tell from the name alone how many leading segments
    are package versus enclosing message. So we try each suffix in turn — take
    the first segment that names a module-level class, then walk the rest as
    attributes (into nested classes) — and use the first chain that resolves.
    """
    parts = qualified.split(".")
    for start in range(len(parts)):
        obj = namespace.get(parts[start])
        if obj is None:
            continue
        try:
            for attr in parts[start + 1 :]:
                obj = getattr(obj, attr)
        except AttributeError:
            continue
        return cast("type", obj)
    msg = f"cannot resolve referenced type {qualified!r}"
    raise LookupError(msg)


def _fast_init_defaults(cls: type[Message]) -> "list[object] | None":
    """Dataclass default objects per field, if decode may bypass ``__init__``.

    The fast path constructs instances with ``__new__`` + setattr, so it is
    only observably equivalent for a plain generated dataclass: default
    construction machinery, no custom ``__post_init__``, dataclass fields that
    mirror the descriptor's fields exactly (same names, same order), and a
    plain default on every field. Anything else returns ``None`` and decode
    calls the class normally. The returned defaults are the very objects
    ``__init__`` would assign, so absent wire fields decode identically on
    both paths. Entries for ``default_factory`` fields (repeated/map) are
    ``None`` placeholders — the decoder always builds those accumulators.
    """
    params = getattr(cls, "__dataclass_params__", None)
    fields = getattr(cls, "__dataclass_fields__", None)
    if params is None or fields is None:
        return None
    supported = (
        # The class must be a dataclass itself, not merely inherit one: a
        # plain subclass shares the parent's descriptor but may add its own
        # __init__, which only the normal construction path would run.
        "__dataclass_fields__" in cls.__dict__
        and not params.frozen
        and type(cls).__call__ is type.__call__
        and cls.__new__ is object.__new__
        and getattr(cls, "__post_init__", None) is Message.__post_init__
        and list(fields) == cls.__fastproto__.field_names()
    )
    if not supported:
        return None
    defaults: list[object] = []
    for f in fields.values():
        if f.init and f.default is not MISSING:
            defaults.append(f.default)
        elif f.init and f.default_factory in (list, dict):
            defaults.append(None)  # pre-created by the decoder, never consulted
        else:
            return None
    return defaults


def _ensure_linked(cls: type[Message]) -> None:
    """Resolve enum/message references for ``cls`` and the classes it references.

    Deferred until first use because sibling classes may not exist when the
    decorator runs. ``link`` marks the descriptor linked *before* we recurse,
    which breaks reference cycles (A -> B -> A).
    """
    descriptor = cls.__fastproto__
    if descriptor.is_linked:
        return

    defaults = _fast_init_defaults(cls)
    fast_init = None if defaults is None else (cls, defaults)

    references = descriptor.ref_fields()
    if not references:
        # no enum/message fields — nothing to resolve
        descriptor.link({}, fast_init)
        return

    namespace = vars(sys.modules[cls.__module__])
    resolved = {number: _resolve(namespace, name) for number, name in references}
    descriptor.link(resolved, fast_init)

    for target in resolved.values():
        if hasattr(target, "__fastproto__"):
            _ensure_linked(cast("type[Message]", target))
