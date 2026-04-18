export const isArrayType = (type: string) => /\[[0-9]*\]$/.test(type);

export const parseArrayType = (type: string) => {
  const match = type.match(/^(.*)\[(\d*)\]$/);
  if (!match) return { baseType: type, length: null as number | null };
  const length = match[2] ? Number(match[2]) : null;
  return { baseType: match[1], length: Number.isNaN(length) ? null : length };
};

export const coercePrimitive = (val: string) => {
  if (val === "true") return true;
  if (val === "false") return false;
  return val;
};

export function buildArgsFromInputs(
  inputsList: any[],
  inputs: Record<number, string | string[]>
): unknown[] {
  return inputsList.map((input: any, i: number) => {
    const val = inputs[i];
    if (isArrayType(input.type)) {
      const arr = Array.isArray(val) ? val : [];
      return arr.map((item) => coercePrimitive(item));
    }
    if (Array.isArray(val)) {
      return coercePrimitive(val.join(","));
    }
    return coercePrimitive(val || "");
  });
}
