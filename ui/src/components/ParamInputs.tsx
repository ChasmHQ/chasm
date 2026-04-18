import { isArrayType, parseArrayType } from "../utils/abiUtils";

interface ParamInputsProps {
  abiInputs: any[];
  values: Record<number, string | string[]>;
  onChange: (next: Record<number, string | string[]>) => void;
}

export function ParamInputs({ abiInputs, values, onChange }: ParamInputsProps) {
  if (abiInputs.length === 0) {
    return (
      <div className="text-slate-600 italic text-sm py-4">
        No parameters required.
      </div>
    );
  }

  return (
    <div className="grid grid-cols-1 gap-5">
      {abiInputs.map((input: any, i: number) => {
        const isArray = isArrayType(input.type);
        const { baseType, length } = parseArrayType(input.type);
        const rawVal = values[i];
        const arrayValues = Array.isArray(rawVal) ? rawVal : [];
        const displayValues =
          length !== null
            ? Array.from({ length }, (_, idx) => arrayValues[idx] ?? "")
            : arrayValues.length > 0
            ? arrayValues
            : [""];

        return (
          <div key={i} className="flex flex-col gap-1.5">
            <label className="text-xs font-medium text-slate-400">
              {input.name || `param_${i}`}{" "}
              <span className="text-slate-600 ml-1">({input.type})</span>
            </label>
            {isArray ? (
              <div className="space-y-2">
                {displayValues.map((val, idx) => (
                  <div key={idx} className="flex items-center gap-2">
                    <input
                      className="flex-1 bg-transparent border-b border-slate-700 py-2 px-1 text-sm text-slate-200 focus:border-indigo-500 outline-none transition-colors placeholder:text-slate-700"
                      placeholder={`Item ${idx + 1} (${baseType})`}
                      value={val}
                      onChange={(e) => {
                        const next = [...displayValues];
                        next[idx] = e.target.value;
                        onChange({ ...values, [i]: next });
                      }}
                    />
                    {length === null && (
                      <button
                        onClick={() => {
                          const next = displayValues.filter((_, index) => index !== idx);
                          onChange({ ...values, [i]: next.length ? next : [""] });
                        }}
                        className="text-[10px] uppercase font-bold text-slate-500 hover:text-red-400"
                        title="Remove item"
                      >
                        Remove
                      </button>
                    )}
                  </div>
                ))}
                {length === null && (
                  <button
                    onClick={() =>
                      onChange({ ...values, [i]: [...displayValues, ""] })
                    }
                    className="text-[10px] uppercase font-bold text-slate-400 hover:text-indigo-300"
                  >
                    Add item
                  </button>
                )}
              </div>
            ) : (
              <input
                className="bg-transparent border-b border-slate-700 py-2 px-1 text-sm text-slate-200 focus:border-indigo-500 outline-none transition-colors placeholder:text-slate-700"
                placeholder={`Enter ${input.type}`}
                value={typeof values[i] === "string" ? values[i] : ""}
                onChange={(e) => onChange({ ...values, [i]: e.target.value })}
              />
            )}
          </div>
        );
      })}
    </div>
  );
}
