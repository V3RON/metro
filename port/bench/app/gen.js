// Generates a synthetic app: N screens/components with JSX, hooks, imports from libs and each other.
const fs=require('fs'),path=require('path');
const N=+process.argv[2]||1500;
fs.rmSync('src',{recursive:true,force:true});
const libs=["import {View, Text, Pressable, StyleSheet, FlatList} from 'react-native';","import {useQuery} from '@tanstack/react-query';","import {format, addDays} from 'date-fns';","import {z} from 'zod';","import {produce} from 'immer';","import {useSelector} from 'react-redux';","import {useTranslation} from 'react-i18next';","import {Svg, Circle} from 'react-native-svg';","import debounce from 'lodash/debounce';"];
for(let i=0;i<N;i++){
  const dir=`src/feature${i%40}`;fs.mkdirSync(dir,{recursive:true});
  const deps=[];for(let k=1;k<=3;k++){const j=i-k*7;if(j>=0)deps.push(j);}
  const imports=deps.map(j=>`import C${j} from '../feature${j%40}/Comp${j}';`).join('\n');
  const body=`import React, {useState, useMemo, useCallback} from 'react';
${libs.slice(0,1+(i%libs.length)).join('\n')}
${imports}

type Props = {title?: string; items?: Array<{id: string; label: string}>};

const schema${i} = ${i%libs.length>=3?`z.object({id: z.string(), count: z.number()})`:'null'};

export default function Comp${i}({title = 'Comp${i}', items = []}: Props) {
  const [count, setCount] = useState(${i});
  const doubled = useMemo(() => items.map(x => ({...x, label: x.label + count})), [items, count]);
  const onPress = useCallback(async () => {
    const next = await Promise.resolve(count + 1);
    setCount(c => c + next);
  }, [count]);
  const {a, ...rest} = {a: 1, b: 2, c: 3};
  const label = \`\${title}: \${count} \${Object.keys(rest).length}\`;
  class Helper${i} { static x = 1; #priv = 2; get value() { return this.#priv + Helper${i}.x; } }
  return (
    <View style={styles.container}>
      <Pressable onPress={onPress}><Text style={styles.text}>{label}{new Helper${i}().value}</Text></Pressable>
      {doubled.map(d => <Text key={d.id}>{d.label ?? 'none'}</Text>)}
      ${deps.map(j=>`<C${j} title="nested" />`).join('\n      ')}
    </View>
  );
}

const styles = StyleSheet.create({
  container: {flex: 1, padding: ${i%16}},
  text: {fontSize: ${12+(i%6)}, color: '#${(i*2654435761%0xffffff).toString(16).padStart(6,'0')}'},
});
`;
  fs.writeFileSync(`${dir}/Comp${i}.tsx`,body);
}
const roots=[];for(let i=N-1;i>=Math.max(0,N-40);i--)roots.push(i);
fs.writeFileSync('index.js',`import {AppRegistry} from 'react-native';
import * as dateFns from 'date-fns';
import * as rx from 'rxjs';
import _ from 'lodash';
import axios from 'axios';
import {configureStore} from '@reduxjs/toolkit';
import i18n from 'i18next';
${roots.map(i=>`import C${i} from './src/feature${i%40}/Comp${i}';`).join('\n')}
const all = [${roots.map(i=>'C'+i).join(',')}];
globalThis.__keep = [dateFns, rx, _, axios, configureStore, i18n, all];
AppRegistry.registerComponent('Bench', () => all[0]);
`);
console.log('generated',N);
