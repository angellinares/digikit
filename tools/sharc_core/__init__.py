"""SHARC+ core semantics shared by the symbolic tracer (tools/sharc_trace.py)
and the concrete runner (tools/sharc_run.py).

Layers, lowest first; a module imports only from modules before it:
  encoding   Register codes, reset values, flag bits, access widths and instruction-field access.
  values     Concrete and symbolic values and the integer arithmetic over them.
  state      Machine state, register access and the trace event log.
  memory     Data memory reads and writes, DAG address arithmetic and SIMD companions.
  floats     32-bit IEEE float and fixed/float conversion helpers.
  flags      ASTATX flag updates for compute results.
  compute_alu    Fixed and float ALU compute (cu=0) operation bodies.
  compute_mult   Multiplier compute (cu=1) operation bodies and MR data moves.
  compute_shift  Shifter compute (cu=2) operation bodies and the ShiftImm form.
  compute_multi  Multifunction and short compute operation bodies.
  compute    Compute operations (ALU, multiplier, shifter, multifunction) and their application.
  sequencer  Decode, program flow, conditions, delayed branches, calls, returns and loops.
  forms      Per-form instruction execution: the _execute dispatch.

The code keeps to a translatable subset (SUBSET.md in this directory,
checked by tools/sharc_subset_lint.py): values.py is the concrete/symbolic
boundary, and everything above it is plain enough to translate to a
native concrete core.
"""
