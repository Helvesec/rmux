"""Subnormal range regression against the independently frozen tmux replies."""
import argparse
import numeric

negative='-0x1.0000000000000001p-1075'
numeric.COLOURS=[
 f'cmy:{negative}/0/0',f'cmy:0/{negative}/0',f'cmy:0/0/{negative}',
 f'cmyk:{negative}/0/0/0',f'cmyk:0/{negative}/0/0',f'cmyk:0/0/{negative}/0',f'cmyk:0/0/0/{negative}',
 'cmy:-0x1p-1076/0/0','cmy:-0x1p-1075/0/0','cmy:-0x1p-1074/0/0',
 'cmy:0x1.0000000000000001p-1075/0/0',
 'cmy:-0x1.0000000000001p-1075/0/0',
 'cmy:-0x1.00000000000008p-1075/0/0',
 'cmy:0x1.00000000000008000000000000000001/0/0',
 'cmy:0x0p99999999999999999999/0/0',
 'cmy:0x1p-99999999999999999999/0/0',
 'cmy:0x1p99999999999999999999/0/0',
 'cmy:0/0.5/1','cmyk:0/0/0/0.5',
]

if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--engine', choices=['oracle', 'candidate'], required=True)
    parser.add_argument('--binary', required=True)
    parser.add_argument('--output', required=True)
    parser.add_argument('--freeze')
    numeric.run(parser.parse_args())
