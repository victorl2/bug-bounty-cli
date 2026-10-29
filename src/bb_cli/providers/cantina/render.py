"""Plain terminal rendering; machine output remains lossless JSON."""
import re

def label(key):
    return re.sub(r'([a-z])([A-Z])',r'\1 \2',key).replace('_',' ').capitalize()

def render(value, indent=0):
    pad=' '*indent
    if isinstance(value,dict):
        for key,item in value.items():
            if item is None: continue
            if isinstance(item,(dict,list)):
                print(pad+label(key)+':')
                render(item,indent+2)
            elif isinstance(item,str) and '\n' in item:
                print(pad+label(key)+':\n'+item+'\n')
            else:
                print(pad+label(key)+': '+('yes' if item is True else 'no' if item is False else str(item)))
    elif isinstance(value,list):
        if not value: print(pad+'(none)')
        for item in value:
            if isinstance(item,(dict,list)):
                render(item,indent)
                print()
            else: print(pad+'- '+str(item))
    else: print(pad+str(value))
