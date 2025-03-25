const HtmlWebpackPlugin = require('html-webpack-plugin');
const HtmlInlineScriptPlugin = require('html-inline-script-webpack-plugin');
const path = require("path");
const { merge } = require('webpack-merge');
const ESLintWebpackPlugin = require('eslint-webpack-plugin');

const config = {
  output: {
    publicPath: "/",
    path: path.resolve(__dirname, "public"),
    filename: "[name].[contenthash].js",
    chunkFilename: '[name].bundle.js'
  },
  optimization: {
    splitChunks: {
      chunks: "all"
    }
  },
  module: {
    rules: [
      {
        test: /\.tsx?$/,
        use: "ts-loader",
        exclude: /node_modules/
      },
      {
        test: /\.(vert|frag|glsl)$/,
        use: "webpack-glsl-loader"
      },
      // SCSS handled in prod/dev configs
    ]
  },
  resolve: {
    extensions: [".tsx", ".ts", ".js"]
  },
};

module.exports = merge(config, {
  entry: {
    main: "./src/index.tsx",
  },
  plugins: [
    new HtmlWebpackPlugin({
      filename: 'index.html',
      template: 'src/index.template.html',
    }),
    new HtmlInlineScriptPlugin(),
    new ESLintWebpackPlugin(),
  ]
});
