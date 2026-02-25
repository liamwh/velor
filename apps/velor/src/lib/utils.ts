import { type ClassValue, clsx } from "clsx";
import { twMerge } from "tailwind-merge";

/**
 * Utility function to merge Tailwind CSS classes with proper precedence.
 * This combines clsx for conditional class names and tailwind-merge for
 * resolving Tailwind class conflicts.
 *
 * @param inputs - Class values to merge
 * @returns Merged class string
 */
export function cn(...inputs: ClassValue[]): string {
	return twMerge(clsx(inputs));
}
